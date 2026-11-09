//! `agent-task fanout cook-batch` and `agent-task batch-prepare`.
//!
//! A cook-batch is a `homeboy/workflow/v1` instance. This module only compiles
//! the operator's one-line batch into that definition and admits it; the
//! workflow driver runs it. The definition has two steps:
//!
//! - `prepare` runs `agent-task batch-prepare`: it resolves the base once,
//!   names each child's branch and worktree, renders each child's prompt, and
//!   transfers the base commit to the Lab runner once so every child installs
//!   it from the runner's object cache instead of shipping its own bundle.
//! - `cook` fans out one `agent-task cook` per child, at most
//!   `--max-concurrency` at a time, and its `join` decides the batch outcome.
//!
//! Status, per-child progress, and cancellation are the workflow's own
//! (`homeboy runs show <instance>` / `homeboy workflow cancel <instance>`).
//! `homeboy agent-task status|diagnose <instance>` rolls every declared child
//! up from those same records: its run and verdict, or `never_started` with
//! the reason it never got a run record.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::core::workflow::definition::Join;
use homeboy::core::workflow::{WorkflowDefinition, WORKFLOW_SCHEMA};
use homeboy::core::{Error, Result};
use serde_json::{json, Value};

use crate::cli_surface::Placement;

use super::super::CmdResult;
use super::args::{
    AgentTaskBatchPrepareArgs, AgentTaskFanoutArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, CookBatchJoin,
};
use super::default_branch::{resolve_default_branch, DefaultBranchRequest};

pub const COOK_BATCH_SCHEMA: &str = "homeboy/agent-task-cook-batch/v1";
pub const BATCH_PREPARE_SCHEMA: &str = "homeboy/agent-task-batch-prepare/v1";
pub const COOK_BATCH_STATUS_SCHEMA: &str = "homeboy/agent-task-cook-batch-status/v1";
const COOK_BATCH_WORKFLOW_ID: &str = "cook-batch";

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    fanout_with_placement(args, Placement::Auto)
}

pub(crate) fn fanout_with_placement(
    args: AgentTaskFanoutArgs,
    placement: Placement,
) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::CookBatch(args) => cook_batch(*args, placement),
    }
}

fn cook_batch(mut args: AgentTaskFanoutCookBatchArgs, placement: Placement) -> CmdResult<Value> {
    let children = parse_issues(&args.issues)?;
    let selection = match args.repo.as_deref() {
        Some(repo) => repo.to_string(),
        None => {
            let repositories = children
                .iter()
                .map(|issue| {
                    issue
                        .key
                        .split('#')
                        .next()
                        .expect("issue key")
                        .to_ascii_lowercase()
                })
                .collect::<BTreeSet<_>>();
            if repositories.len() != 1 {
                return Err(Error::validation_invalid_argument(
                    "repo",
                    "cannot infer --repo: every issue URL must belong to one GitHub repository; pass --repo explicitly",
                    None,
                    None,
                ));
            }
            repositories.into_iter().next().expect("one repository")
        }
    };
    if !args.cook.gates.has_deterministic_gate() && !args.cook.repro.is_declared() {
        return Err(Error::validation_invalid_argument(
            "verify",
            "every cook-batch child needs a deterministic gate: pass --verify, --verify-file, --private-verify, --private-verify-file, --repro, or --repro-file",
            None,
            None,
        ));
    }
    // Every child shares these gates and the Cook spine rejects a reviewer-
    // unsafe one; reject it here, file gates included, before any wave work
    // exists (#15672).
    let mut visible = args.cook.gates.clone();
    visible.snapshot_file_inputs()?;
    homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&visible.verify, None)?;
    let mut repro = args.cook.repro.clone();
    repro.snapshot_file_inputs()?;
    homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&repro.repro, None)?;
    if let CookBatchJoin::MinSucceeded(min) = args.join {
        if min as usize > children.len() {
            return Err(Error::validation_invalid_argument(
                "join",
                format!(
                    "--join {min} needs more children than the {} issues given",
                    children.len()
                ),
                Some(min.to_string()),
                None,
            ));
        }
    }
    let (_, component) = super::run::cook_repository_names_for_selection(&selection)?;
    args.repo = Some(component.clone());
    let bundle_runner = match placement {
        Placement::Lab | Placement::LabOrLocal => lab_runner()?,
        Placement::Auto | Placement::Local => None,
    };
    let workspace =
        super::run::cook_component_path_for_repository_name(&component)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "repo",
                "Cook gate preflight requires a repository checkout",
                Some(selection),
                None,
            )
        })?;
    let mut gates = visible.into();
    super::run::preflight_resolved_cook_gates(&mut gates, Path::new(&workspace))?;
    let definition = compile(&args, placement, bundle_runner.as_deref())?;
    super::super::workflow::validate(&definition)?;
    let instance = definition.instance_id(&BTreeMap::new(), args.fanout_id.as_deref());
    if !args.run_plan {
        return Ok((
            json!({
                "schema": COOK_BATCH_SCHEMA,
                // Planned, not run: the envelope reports `planned` (#13702).
                "status": "ready",
                "mode": "preview",
                "instance": instance,
                "children": children.len(),
                "max_parallel": definition.steps[1].max_parallel,
                "join": definition.steps[1].join,
                "bundle_runner": bundle_runner,
                "workflow": definition,
                "run_command": "re-run this command with --run-plan to admit the workflow",
            }),
            0,
        ));
    }
    let (mut view, code) = super::super::workflow::admit(
        &definition,
        &BTreeMap::new(),
        args.fanout_id.as_deref(),
        false,
        "agent-task fanout cook-batch",
    )?;
    view["schema"] = json!(COOK_BATCH_SCHEMA);
    Ok((view, code))
}

/// The Lab runner the children are placed on, whose workspace bundle the
/// batch prepares once.
fn lab_runner() -> Result<Option<String>> {
    if let Some(runner) = homeboy::core::parsed_command_preflight::captured_result()
        .and_then(|preflight| preflight.placement.runner)
    {
        return Ok(Some(runner.runner_id));
    }
    homeboy::runner::resolve_default_lab_runner()
}

/// Compile a cook-batch invocation into its workflow definition.
pub(super) fn compile(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
    bundle_runner: Option<&str>,
) -> Result<WorkflowDefinition> {
    let mut prepare = vec![
        "agent-task".to_string(),
        "batch-prepare".to_string(),
        "--repo".to_string(),
        args.repo.clone().ok_or_else(|| {
            Error::validation_missing_argument(vec![
                "--repo must be resolved before compiling a cook-batch".to_string(),
            ])
        })?,
        "--branch-prefix".to_string(),
        args.branch_prefix.clone(),
    ];
    for (flag, value) in [
        ("--base", args.base.as_ref()),
        ("--prompt-template", args.prompt_template.as_ref()),
        (
            "--bundle-runner",
            bundle_runner.map(str::to_string).as_ref(),
        ),
    ] {
        if let Some(value) = value {
            prepare.extend([flag.to_string(), value.clone()]);
        }
    }
    prepare.extend(args.issues.iter().cloned());

    let mut cook = args.cook.child_flags()?;
    cook.extend([
        ("repo".into(), json!(args.repo)),
        ("task-url".into(), json!("{{item.issue_url}}")),
        ("prompt".into(), json!("{{item.prompt}}")),
        ("head".into(), json!("{{item.branch}}")),
        ("to-worktree".into(), json!("{{item.worktree}}")),
        ("base".into(), json!("{{steps.prepare.output/base}}")),
    ]);
    if placement != Placement::Auto {
        cook.insert("placement".into(), json!(placement_name(placement)));
    }

    let max_parallel = args
        .max_concurrency
        .map(|max| max as usize)
        .or(homeboy::core::defaults::load_config()
            .agent_task
            .max_batch_concurrency)
        .unwrap_or(homeboy::core::defaults::DEFAULT_AGENT_TASK_MAX_BATCH_CONCURRENCY)
        .max(1);
    let definition = json!({
        "schema": WORKFLOW_SCHEMA,
        "id": COOK_BATCH_WORKFLOW_ID,
        // Credentials a child's provider reads are secret references the
        // daemon resolves when each step spawns, never inherited values.
        "env": { "secrets": args.cook.secret_env },
        "budget": args.max_duration.map(|seconds| json!({ "deadline": format!("{seconds}s") })),
        "steps": [
            { "id": "prepare", "homeboy": prepare },
            {
                "id": "cook",
                "needs": ["prepare"],
                "foreach": "{{steps.prepare.output/children}}",
                "max_parallel": max_parallel,
                "join": Value::from(match args.join {
                    CookBatchJoin::All => Join::All,
                    CookBatchJoin::Any => Join::Any,
                    CookBatchJoin::MinSucceeded(min) => Join::MinSucceeded(min),
                }),
                "cook": cook,
            },
        ],
    });
    let mut definition = definition;
    if definition["budget"].is_null() {
        definition.as_object_mut().expect("object").remove("budget");
    }
    WorkflowDefinition::from_value(definition)
}

fn placement_name(placement: Placement) -> String {
    clap::ValueEnum::to_possible_value(&placement)
        .expect("placement has a clap value")
        .get_name()
        .to_string()
}

/// One GitHub issue of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IssueRef {
    url: String,
    repo: String,
    number: String,
    key: String,
}

impl IssueRef {
    fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim();
        let invalid = |message: &str| {
            Error::validation_invalid_argument("issues", message, Some(trimmed.to_string()), None)
        };
        let (prefix, number_part) = trimmed
            .split_once("/issues/")
            .ok_or_else(|| invalid("cook-batch issue inputs must be GitHub issue URLs"))?;
        let number = number_part
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(invalid(
                "GitHub issue URL is missing a numeric issue number",
            ));
        }
        let repository = prefix
            .strip_prefix("https://github.com/")
            .ok_or_else(|| invalid("cook-batch issue inputs must be GitHub issue URLs"))?;
        let mut segments = repository.split('/');
        let owner = segments.next().unwrap_or_default();
        let repo = segments.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() || segments.next().is_some() {
            return Err(invalid("GitHub issue URL is missing its owner/repo"));
        }
        Ok(Self {
            url: trimmed.to_string(),
            repo: repo.to_string(),
            number: number.to_string(),
            key: format!("{owner}/{repo}#{number}"),
        })
    }
}

fn parse_issues(issues: &[String]) -> Result<Vec<IssueRef>> {
    let mut seen = BTreeSet::new();
    issues
        .iter()
        .map(|url| {
            let issue = IssueRef::parse(url)?;
            if !seen.insert(issue.key.clone()) {
                return Err(Error::validation_invalid_argument(
                    "issues",
                    "duplicate issue URLs are not allowed in one cook-batch",
                    Some(url.clone()),
                    None,
                ));
            }
            Ok(issue)
        })
        .collect()
}

/// `agent-task batch-prepare`: plan every child of a batch once.
pub(super) fn batch_prepare(args: AgentTaskBatchPrepareArgs) -> CmdResult<Value> {
    let issues = parse_issues(&args.issues)?;
    let (repository, component) = super::run::cook_repository_names_for_selection(&args.repo)?;
    let path =
        super::run::cook_component_path_for_repository_name(&component)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "repo",
                format!("--repo `{}` has no registered checkout", args.repo),
                Some(args.repo.clone()),
                None,
            )
        })?;
    let resolution = resolve_default_branch(
        DefaultBranchRequest {
            explicit_base: args.base.as_deref(),
            explicit_from: None,
            workspace: None,
            component: Some(&path),
            destination: None,
        },
        None,
    )?;
    // Resolve the base once: every child is cut from this exact commit.
    let _ = git(&path, &["fetch", "--quiet", "origin", &resolution.base]);
    let base_sha = git(
        &path,
        &["rev-parse", &format!("{}^{{commit}}", resolution.from)],
    )?;

    let children = issues
        .iter()
        .map(|issue| {
            let branch = format!(
                "{}/issue-{}-{}",
                args.branch_prefix.trim_matches('/'),
                issue.number,
                slugify(&issue.repo)
            );
            let worktree = format!("{repository}@{}", slugify(&branch));
            let prompt = match &args.prompt_template {
                Some(template) => template
                    .replace("{issue_url}", &issue.url)
                    .replace("{issue_ref}", &issue.key)
                    .replace("{repo}", &repository)
                    .replace("{branch}", &branch)
                    .replace("{worktree}", &worktree),
                // Homeboy authors no agent-facing prose (#15818): the issue
                // itself is the task.
                None => issue.url.clone(),
            };
            json!({
                "issue_url": issue.url,
                "issue_ref": issue.key,
                "branch": branch,
                "worktree": worktree,
                "prompt": prompt,
            })
        })
        .collect::<Vec<_>>();
    let bundle = match &args.bundle_runner {
        Some(runner) => prepare_workspace_bundle(runner, &path, &base_sha),
        None => {
            json!({ "status": "not_applicable", "reason": "the children are not placed on a Lab runner" })
        }
    };
    Ok((
        json!({
            "schema": BATCH_PREPARE_SCHEMA,
            "repo": repository,
            "component": component,
            "base": resolution.base,
            "base_sha": base_sha,
            "default_branch_resolution": resolution,
            "children": children,
            "workspace_bundle": bundle,
        }),
        0,
    ))
}

/// Transfer the batch's base commit to the Lab runner once. Every child at
/// that commit then installs it from the runner's object cache instead of
/// shipping its own bundle (#15677). A failure here costs only the reuse:
/// each child still transfers its own bundle.
fn prepare_workspace_bundle(runner: &str, repository: &Path, base_sha: &str) -> Value {
    let staging = match tempfile::Builder::new()
        .prefix("homeboy-batch-bundle-")
        .tempdir()
    {
        Ok(staging) => staging,
        Err(error) => return bundle_failure(runner, base_sha, error.to_string()),
    };
    let checkout: PathBuf = staging.path().join(format!("base-{}", &base_sha[..12]));
    let checkout_arg = checkout.display().to_string();
    if let Err(error) = git(
        repository,
        &[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            &checkout_arg,
            base_sha,
        ],
    ) {
        return bundle_failure(runner, base_sha, error.message);
    }
    let synced = homeboy::runner::runners::sync_workspace(
        runner,
        homeboy::runner::runners::RunnerWorkspaceSyncOptions {
            path: checkout_arg.clone(),
            mode: homeboy::runner::runners::RunnerWorkspaceSyncMode::SnapshotGit,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            validation_dependency_ids: None,
            run_isolation_token: None,
        },
    );
    let _ = git(
        repository,
        &["worktree", "remove", "--force", &checkout_arg],
    );
    match synced {
        Ok((output, _)) => json!({
            "status": "prepared",
            "runner": runner,
            "source_sha": base_sha,
            "remote_path": output.remote_path,
            "transfer": output.materialization_plan.controller_git_bundle,
        }),
        Err(error) => bundle_failure(runner, base_sha, error.message),
    }
}

fn bundle_failure(runner: &str, base_sha: &str, error: String) -> Value {
    json!({
        "status": "failed",
        "runner": runner,
        "source_sha": base_sha,
        "error": homeboy::core::redaction::redact_string(&error),
    })
}

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), Some("run git".into())))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether an `agent-task status`/`diagnose` id names a workflow instance.
pub(super) fn is_workflow_instance(id: &str) -> bool {
    id.starts_with("wf-")
}

/// `agent-task status|diagnose <wf-id>`: read the instance from the daemon
/// and roll its children up.
pub(super) fn batch_rollup_from_daemon(instance: &str, operation: &str) -> CmdResult<Value> {
    let client = homeboy::core::daemon::LocalControllerJobClient::connect()?;
    batch_rollup(instance, operation, |id| client.workflow(id))
}

/// The per-child rollup of a cook-batch instance, built only from the
/// workflow's durable item records and each child's lifecycle record. Every
/// declared child is listed; a child with no run record is `never_started`
/// with the reason.
pub(super) fn batch_rollup(
    instance: &str,
    operation: &str,
    read_workflow: impl FnOnce(&str) -> Result<Value>,
) -> CmdResult<Value> {
    let view = read_workflow(instance)?;
    if view["workflow"] != COOK_BATCH_WORKFLOW_ID {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "`{instance}` is a `{}` workflow, not a cook-batch; read it with `homeboy runs show {instance}`",
                view["workflow"].as_str().unwrap_or("unknown")
            ),
            Some(instance.to_string()),
            None,
        ));
    }
    let status = view["status"].as_str().unwrap_or("unknown");
    let terminal = !matches!(status, "running" | "queued");
    let batch_reason = view["reason"].as_str().unwrap_or(status);
    let cook = &view["steps"]["cook"];
    let children = cook["items"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|item| batch_child(instance, item, status, batch_reason))
        .collect::<Vec<_>>();
    let never_started = children
        .iter()
        .filter(|child| child["state"] == homeboy::core::workflow::advance::NEVER_STARTED)
        .count();
    let mut rollup = json!({
        "schema": COOK_BATCH_STATUS_SCHEMA,
        "operation": operation,
        "instance": instance,
        "status": status,
        "reason": view["reason"],
        "children_total": children.len(),
        "never_started": never_started,
        "children": children,
        "workflow_command": format!("homeboy runs show {instance}"),
    });
    if cook["items"].as_array().map_or(true, Vec::is_empty) {
        rollup["children_reason"] = json!(if terminal {
            format!("the batch ended before it declared its children: {batch_reason}")
        } else {
            "the batch has not declared its children yet".to_string()
        });
    }
    Ok((rollup, 0))
}

fn batch_child(instance: &str, item: &Value, status: &str, batch_reason: &str) -> Value {
    let terminal = !matches!(status, "running" | "queued");
    let index = item["index"].as_u64().unwrap_or_default();
    let cook_id = item["run_id"].as_str();
    let mut child = json!({
        "index": index,
        "child_id": cook_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("{instance}/cook/item-{index}")),
        "task_url": item["item"]["issue_url"],
        "issue_ref": item["item"]["issue_ref"],
        "run_id": Value::Null,
        "verdict": Value::Null,
        "candidate": Value::Null,
        "pr_url": Value::Null,
    });
    if let Some(run) = cook_id.and_then(child_run) {
        for (key, value) in run {
            child[key] = value;
        }
        return child;
    }
    let outcome = item["outcome"].as_str();
    let error = item["error"]
        .as_str()
        .filter(|error| !error.trim().is_empty());
    let settled = item["phase"] == "done";
    if !terminal && !settled {
        // Still waiting for a slot or for its Cook to write its first record.
        child["state"] = json!(if cook_id.is_some() {
            "admitting"
        } else {
            "pending"
        });
        child["next_action"] = json!(format!("homeboy runs watch {instance}"));
        return child;
    }
    let reason = match (outcome, error) {
        (Some(homeboy::core::workflow::advance::NEVER_STARTED), Some(error)) => error.to_string(),
        (_, Some(error)) => format!("admission refused: {error}"),
        (Some("cancelled"), None) | (None, None) if status == "cancelled" => {
            format!("cancelled before admission: {batch_reason}")
        }
        (Some(outcome), None) if cook_id.is_some() => format!(
            "admission refused: the child's Cook ended `{outcome}` before it wrote a run record"
        ),
        _ => format!("batch failed first: {batch_reason}"),
    };
    child["state"] = json!(homeboy::core::workflow::advance::NEVER_STARTED);
    child["reason"] = json!(reason);
    child["next_action"] = json!(match child["task_url"].as_str() {
        Some(url) => format!("re-run this child: homeboy agent-task fanout cook-batch {url} ..."),
        None => format!("homeboy runs show {instance}"),
    });
    child
}

/// The durable run behind a child's Cook id, or `None` when it never wrote
/// one.
fn child_run(cook_id: &str) -> Option<Vec<(&'static str, Value)>> {
    use homeboy::agents::agent_tasks::lifecycle;
    let store = lifecycle::AgentTaskLifecycleStore::from_environment().ok()?;
    let target =
        homeboy::agents::agent_task_lifecycle::resolve_cook_reader_target_in_store(&store, cook_id)
            .ok()?;
    let record = homeboy::agents::agent_task_lifecycle::exact_record_bounded_in_store(
        &store,
        &target.run_id,
    )
    .ok()?;
    let run = homeboy::agents::agent_tasks::service::control_plane_run(&record.run_id).ok()?;
    let verdict = crate::commands::cook_verdict::cook_verdict(&run);
    let pr_url =
        homeboy::agents::agent_task_service::CookDelivery::from_run_metadata(&record.metadata)
            .as_ref()
            .and_then(|delivery| delivery.pr_url().map(str::to_string));
    let candidate = record
        .candidate
        .as_ref()
        .map(|candidate| json!(candidate))
        .or_else(|| verdict.candidate.as_ref().map(|state| json!(state)))
        .unwrap_or(Value::Null);
    Some(vec![
        ("run_id", json!(record.run_id)),
        ("state", json!(verdict.state)),
        ("verdict", json!(verdict.state)),
        ("reason", json!(verdict.reason)),
        ("candidate", candidate),
        ("pr_url", json!(pr_url)),
        ("next_action", json!(verdict.next_action)),
    ])
}

fn slugify(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    fn parse(argv: &[&str]) -> AgentTaskFanoutCookBatchArgs {
        let mut full = vec!["homeboy", "agent-task", "fanout", "cook-batch"];
        full.extend_from_slice(argv);
        let cli = Cli::try_parse_from(full).expect("parse cook-batch");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task");
        };
        let super::super::AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout");
        };
        let AgentTaskFanoutCommand::CookBatch(args) = fanout.command;
        *args
    }

    const ISSUES: [&str; 3] = [
        "https://github.com/chubes4/homeboy-cook-scratch/issues/1",
        "https://github.com/chubes4/homeboy-cook-scratch/issues/2",
        "https://github.com/chubes4/homeboy-cook-scratch/issues/3",
    ];

    /// #16273: batch-prepare admits the `owner/repo` form a single Cook admits,
    /// instead of reading it as a checkout path.
    #[test]
    fn batch_prepare_resolves_owner_repo_like_a_single_cook() {
        crate::test_support::with_isolated_home(|home| {
            let origin = home.path().join("origin.git");
            let checkout = home.path().join("extrachill-events");
            let run = |dir: &Path, args: &[&str]| {
                let status = std::process::Command::new("git")
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args)
                    .current_dir(dir)
                    .status()
                    .unwrap();
                assert!(status.success(), "git {args:?}");
            };
            run(
                home.path(),
                &["init", "-q", "--bare", "-b", "main", "origin.git"],
            );
            run(
                home.path(),
                &["clone", "-q", origin.to_str().unwrap(), "extrachill-events"],
            );
            std::fs::write(checkout.join("README"), "x").unwrap();
            run(&checkout, &["add", "README"]);
            run(&checkout, &["commit", "-qm", "init"]);
            run(&checkout, &["push", "-q", "origin", "HEAD:main"]);
            homeboy::core::component::write_standalone_component_config(
                &homeboy::core::component::Component {
                    id: "extrachill-events".to_string(),
                    local_path: checkout.display().to_string(),
                    remote_url: Some(
                        "https://github.com/Extra-Chill/extrachill-events.git".to_string(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();

            let (value, code) = batch_prepare(AgentTaskBatchPrepareArgs {
                issues: vec!["https://github.com/Extra-Chill/extrachill-events/issues/12".into()],
                repo: "Extra-Chill/extrachill-events".to_string(),
                base: Some("main".to_string()),
                branch_prefix: "fix".to_string(),
                prompt_template: None,
                bundle_runner: None,
            })
            .expect("owner/repo is admitted");
            assert_eq!(code, 0);
            assert_eq!(value["component"], "extrachill-events");
            assert_eq!(value["base"], "main");
            assert_eq!(value["children"].as_array().map(Vec::len), Some(1));

            let (preview, _) = cook_batch(
                parse(&[
                    "https://github.com/Extra-Chill/extrachill-events/issues/12",
                    "--verify",
                    "true",
                    "--preview",
                ]),
                Placement::Local,
            )
            .expect("infer registered component from issue URL");
            assert_eq!(
                preview["workflow"]["steps"][1]["cook"]["repo"],
                "extrachill-events"
            );

            let error = cook_batch(
                parse(&[
                    "https://github.com/Extra-Chill/extrachill-events/issues/12",
                    "--repo",
                    "nope/nope",
                    "--verify",
                    "true",
                ]),
                Placement::Local,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("Registered components"), "{error}");
            assert!(error.contains("extrachill-events"), "{error}");
            assert!(
                error.contains("https://github.com/Extra-Chill/extrachill-events.git"),
                "{error}"
            );
            assert!(error.contains(checkout.to_str().unwrap()), "{error}");
        });
    }

    #[test]
    fn cook_batch_inference_rejects_mixed_or_unregistered_repositories() {
        crate::test_support::with_isolated_home(|_| {
            let mixed = cook_batch(
                parse(&[
                    ISSUES[0],
                    "https://github.com/other/repo/issues/1",
                    "--verify",
                    "true",
                ]),
                Placement::Local,
            )
            .unwrap_err()
            .to_string();
            assert!(
                mixed.contains("every issue URL must belong to one GitHub repository"),
                "{mixed}"
            );
            let missing = cook_batch(parse(&[ISSUES[0], "--verify", "true"]), Placement::Local)
                .unwrap_err()
                .to_string();
            assert!(missing.contains("Registered components"), "{missing}");
            assert!(IssueRef::parse("https://example.com/owner/repo/issues/1").is_err());
        });
    }

    #[test]
    fn cook_batch_compiles_to_a_validated_fan_out_workflow() {
        let mut argv = vec![
            "--repo",
            "homeboy-cook-scratch",
            "--verify",
            "test -f DONE",
            "--max-concurrency",
            "2",
            "--join",
            "2",
            "--secret-env",
            "SCRATCH_TOKEN",
            "--gate-timeout-seconds",
            "60",
        ];
        argv.extend(ISSUES);
        let args = parse(&argv);
        let definition = compile(&args, Placement::Lab, Some("homeboy-lab")).unwrap();
        super::super::super::workflow::validate(&definition).expect("compiled workflow validates");
        let value = serde_json::to_value(&definition).unwrap();
        assert_eq!(value["id"], "cook-batch");
        assert_eq!(value["env"]["secrets"], json!(["SCRATCH_TOKEN"]));
        let prepare = value["steps"][0]["homeboy"].as_array().unwrap();
        assert_eq!(prepare[..2], [json!("agent-task"), json!("batch-prepare")]);
        assert!(prepare.contains(&json!("--bundle-runner")));
        assert_eq!(
            prepare[prepare.len() - 3..],
            ISSUES.map(|issue| json!(issue))
        );
        let cook = &value["steps"][1];
        assert_eq!(cook["foreach"], "{{steps.prepare.output/children}}");
        assert_eq!(cook["max_parallel"], 2);
        assert_eq!(cook["join"], json!({"min_succeeded": 2}));
        assert_eq!(cook["cook"]["task-url"], "{{item.issue_url}}");
        // #16061: children take title and commit subject from the agent form.
        assert!(cook["cook"].get("title").is_none());
        assert!(cook["cook"].get("commit-message").is_none());
        assert_eq!(cook["cook"]["verify"], json!(["test -f DONE"]));
        assert_eq!(cook["cook"]["placement"], "lab");
        assert_eq!(cook["cook"]["gate-timeout-seconds"], "60");
        assert!(cook["cook"].get("gate-execution-policy").is_none());
        // The same batch names the same instance; a fanout id separates it.
        let instance = definition.instance_id(&BTreeMap::new(), None);
        assert_eq!(
            instance,
            compile(&args, Placement::Lab, Some("homeboy-lab"))
                .unwrap()
                .instance_id(&BTreeMap::new(), None)
        );
        assert!(instance.starts_with("wf-cook-batch-"));
        assert_ne!(
            instance,
            definition.instance_id(&BTreeMap::new(), Some("again"))
        );
    }

    #[test]
    fn relative_gate_file_paths_are_anchored_at_the_operator_directory() {
        let args = parse(&[
            "--repo",
            "scratch",
            "--verify-file",
            "public.sh",
            "--private-verify-file",
            "private.sh",
            ISSUES[0],
        ]);
        let definition = compile(&args, Placement::Auto, None).unwrap();
        let child = definition.steps[1].cook.as_ref().unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            child["verify-file"],
            json!([cwd.join("public.sh").display().to_string()])
        );
        assert_eq!(
            child["private-verify-file"],
            json!([cwd.join("private.sh").display().to_string()])
        );
    }

    #[test]
    fn auto_placement_leaves_children_to_cook_policy_and_join_defaults_to_all() {
        let mut argv = vec!["--repo", "scratch", "--verify", "true"];
        argv.extend(ISSUES);
        let definition = compile(&parse(&argv), Placement::Auto, None).unwrap();
        let value = serde_json::to_value(&definition).unwrap();
        assert!(value["steps"][1]["cook"].get("placement").is_none());
        assert_eq!(value["steps"][1]["join"], "all");
        assert!(!value["steps"][0]["homeboy"]
            .as_array()
            .unwrap()
            .contains(&json!("--bundle-runner")));
    }

    /// #15672: a public `--verify-file` gate naming a machine-local path is
    /// rejected at batch admission, naming the token, before any wave exists.
    #[test]
    fn reviewer_unsafe_verify_file_is_rejected_before_admission() {
        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("gate.sh");
        std::fs::write(&gate, "cargo test > /tmp/be-canonical-steps.$$\n").unwrap();
        let gate = gate.display().to_string();
        let mut argv = vec!["--repo", "scratch", "--verify-file", gate.as_str()];
        argv.extend(ISSUES);
        let error = cook_batch(parse(&argv), Placement::Local).expect_err("unsafe gate");
        let problem = error.details["problem"].as_str().expect("problem text");
        assert!(
            problem.contains("`/tmp/be-canonical-steps.$$` (machine-local absolute path)"),
            "{problem}"
        );

        let mut argv = vec![
            "--repo",
            "scratch",
            "--verify",
            "cargo test",
            "--private-verify",
            "check http://localhost:5432",
        ];
        argv.extend(ISSUES);
        let args = parse(&argv);
        let mut visible = args.cook.gates.clone();
        visible.snapshot_file_inputs().unwrap();
        homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&visible.verify, None)
            .expect("a private gate is never published to reviewers");
    }

    /// #17708: a cancelled cook-batch lists every declared child. The child
    /// that wrote a run record reports its verdict; the one admitted without a
    /// record and the one never submitted are `never_started` with a reason.
    #[test]
    fn cook_batch_status_rolls_up_every_child_including_never_started() {
        use homeboy::core::workflow::advance::{advance, Event, InstanceState, StepResult};
        crate::test_support::with_isolated_home(|_| {
            let instance = "wf-cook-batch-rollup";
            let definition = WorkflowDefinition::from_value(json!({
                "schema": WORKFLOW_SCHEMA,
                "id": COOK_BATCH_WORKFLOW_ID,
                "steps": [
                    {"id": "prepare", "homeboy": ["agent-task", "batch-prepare"]},
                    {"id": "cook", "needs": ["prepare"],
                     "foreach": "{{steps.prepare.output/children}}", "max_parallel": 2,
                     "cook": {"repo": "scratch", "task-url": "{{item.issue_url}}"}}
                ]
            }))
            .unwrap();
            let initial = InstanceState::new(&definition, instance, BTreeMap::new());
            let (state, _) = advance(&definition, &initial, &Event::Start, 0);
            let prepare_key = state.steps["prepare"].key.clone().unwrap();
            let children = ISSUES
                .iter()
                .map(|url| json!({"issue_url": url}))
                .collect::<Vec<_>>();
            let (state, _) = advance(
                &definition,
                &state,
                &Event::StepFinished(StepResult {
                    key: prepare_key,
                    error: None,
                    status: homeboy::core::workflow::advance::FinishedStatus::Succeeded,
                    outcome: None,
                    output: Some(json!({ "children": children })),
                    run_id: None,
                }),
                1,
            );
            let (state, _) = advance(
                &definition,
                &state,
                &Event::Cancel {
                    reason: Some("operator stop".to_string()),
                },
                2,
            );
            let first = state.steps["cook"].items[0].run_id.clone().unwrap();
            let plan =
                homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("child-0", Vec::new());
            homeboy::agents::agent_tasks::lifecycle::submit_plan(&plan, Some(&first))
                .expect("persist first child run");
            let mut view = homeboy::core::daemon::workflow_job::projection(&state, None);
            view["workflow"] = json!(COOK_BATCH_WORKFLOW_ID);

            let (rollup, code) = batch_rollup(instance, "status", |id| {
                assert_eq!(id, instance);
                Ok(view)
            })
            .expect("rollup");
            assert_eq!(code, 0);
            assert_eq!(rollup["schema"], COOK_BATCH_STATUS_SCHEMA);
            assert_eq!(rollup["status"], "cancelled");
            assert_eq!(rollup["children_total"], 3);
            assert_eq!(rollup["never_started"], 2);
            let children = rollup["children"].as_array().unwrap();
            for (child, url) in children.iter().zip(ISSUES) {
                assert_eq!(child["task_url"], url);
                assert!(child["next_action"].is_string(), "{child}");
            }
            assert_eq!(children[0]["run_id"], first);
            assert_ne!(children[0]["state"], "never_started");
            assert!(children[0]["verdict"].is_string());
            for child in &children[1..] {
                assert_eq!(child["state"], "never_started", "{child}");
                assert!(child["run_id"].is_null());
                assert_eq!(
                    child["reason"], "cancelled before admission: operator stop",
                    "{child}"
                );
            }
        });
    }

    #[test]
    fn cook_batch_status_refuses_other_workflows() {
        let error = batch_rollup("wf-other-1", "status", |_| {
            Ok(json!({"workflow": "other", "status": "running"}))
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("not a cook-batch"), "{error}");
    }

    #[test]
    fn issues_are_parsed_once_and_duplicates_refused() {
        let issues = parse_issues(&ISSUES.map(str::to_string)).unwrap();
        assert_eq!(issues[1].key, "chubes4/homeboy-cook-scratch#2");
        assert!(parse_issues(&[ISSUES[0].to_string(), ISSUES[0].to_string()]).is_err());
        assert!(parse_issues(&["https://example.test/pulls/1".to_string()]).is_err());
        assert_eq!(slugify("fix/issue-2-homeboy"), "fix-issue-2-homeboy");
    }
}
