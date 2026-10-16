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
//! (`homeboy workflow status|cancel <instance>`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::agents::agent_task_provider::AgentTaskProviderProfileDeclaration;
use homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog;
use homeboy::core::workflow::definition::Join;
use homeboy::core::workflow::{WorkflowDefinition, WORKFLOW_SCHEMA};
use homeboy::core::{Error, Result};
use serde_json::{json, Map, Value};

use crate::cli_surface::Placement;

use super::super::CmdResult;
use super::args::{
    AgentTaskBatchPrepareArgs, AgentTaskFanoutArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, CookBatchJoin, VerifyGateArgs,
};
use super::default_branch::{resolve_default_branch, DefaultBranchRequest};

pub const COOK_BATCH_SCHEMA: &str = "homeboy/agent-task-cook-batch/v1";
pub const BATCH_PREPARE_SCHEMA: &str = "homeboy/agent-task-batch-prepare/v1";
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
    apply_provider_profile(&mut args);
    let children = parse_issues(&args.issues)?;
    if !args.gates.has_deterministic_gate() {
        return Err(Error::validation_invalid_argument(
            "verify",
            "every cook-batch child needs a deterministic gate: pass --verify, --verify-file, --private-verify, or --private-verify-file",
            None,
            None,
        ));
    }
    // Every child shares these gates and the Cook spine rejects a reviewer-
    // unsafe one; reject it here, file gates included, before any wave work
    // exists (#15672).
    let mut visible = args.gates.clone();
    visible.snapshot_file_inputs()?;
    homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&visible.verify, None)?;
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
    let bundle_runner = match placement {
        Placement::Lab | Placement::LabOrLocal => lab_runner()?,
        Placement::Auto | Placement::Local => None,
    };
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
fn compile(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
    bundle_runner: Option<&str>,
) -> Result<WorkflowDefinition> {
    let mut prepare = vec![
        "agent-task".to_string(),
        "batch-prepare".to_string(),
        "--repo".to_string(),
        args.repo.clone(),
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

    let mut cook = Map::new();
    let mut set = |key: &str, value: Value| {
        cook.insert(key.to_string(), value);
    };
    set("repo", json!(args.repo));
    set("task-url", json!("{{item.issue_url}}"));
    set("prompt", json!("{{item.prompt}}"));
    set("head", json!("{{item.branch}}"));
    set("to-worktree", json!("{{item.worktree}}"));
    set("base", json!("{{steps.prepare.output/base}}"));
    // No title or commit message: each child Cook takes both from its own
    // validated agent review form, exactly like a single Cook (#15122,
    // #16052). A per-item "Fix #N" would override that title (#16061).
    for (key, value) in [
        ("backend", &args.backend),
        ("selector", &args.selector),
        ("model", &args.model),
        ("provider-config", &args.provider_config),
        ("ai-tool", &args.ai_tool),
    ] {
        if let Some(value) = value {
            set(key, json!(value));
        }
    }
    if let Some(max_attempts) = args.max_attempts {
        set("max-attempts", json!(max_attempts));
    }
    if args.acknowledge_model_override {
        set("acknowledge-model-override", json!(true));
    }
    if !args.secret_env.is_empty() {
        set("secret-env", json!(args.secret_env));
    }
    if !args.provider_evidence_inputs.is_empty() {
        set(
            "provider-evidence",
            json!(args
                .provider_evidence_inputs
                .iter()
                .map(serialize)
                .collect::<Result<Vec<_>>>()?),
        );
    }
    for (key, value) in gate_flags(&args.gates)? {
        set(&key, value);
    }
    if placement != Placement::Auto {
        set("placement", json!(placement_name(placement)));
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
        "env": { "secrets": args.secret_env },
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

/// The `agent-task cook` gate flags equivalent to `gates`. Defaults are left
/// to the Cook so one contract owns them.
fn gate_flags(gates: &VerifyGateArgs) -> Result<Vec<(String, Value)>> {
    let defaults = default_gates();
    let mut flags = Vec::new();
    let absolute = |paths: &[String]| -> Result<Vec<String>> {
        let cwd = std::env::current_dir().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("resolve the working directory".into()),
            )
        })?;
        Ok(paths
            .iter()
            .map(|path| cwd.join(path).display().to_string())
            .collect())
    };
    for (key, values) in [
        ("verify", gates.verify.clone()),
        ("verify-file", absolute(&gates.verify_file)?),
        ("private-verify", gates.private_verify.clone()),
        ("private-verify-file", absolute(&gates.private_verify_file)?),
        ("gate-toolchain", gates.gate_toolchains.clone()),
        (
            "gate-env",
            gates
                .gate_environment
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect(),
        ),
        (
            "gate-env-from",
            gates
                .gate_environment_preserve
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect(),
        ),
        (
            "gate-input-source",
            gates
                .input_sources
                .iter()
                .map(serialize)
                .collect::<Result<_>>()?,
        ),
        (
            "gate-toolchain-spec",
            gates
                .gate_toolchain_specs
                .iter()
                .map(serialize)
                .collect::<Result<_>>()?,
        ),
        (
            "gate-package-artifact",
            gates
                .gate_package_artifacts
                .iter()
                .map(serialize)
                .collect::<Result<_>>()?,
        ),
        (
            "gate-extension-input",
            gates
                .gate_extension_inputs
                .iter()
                .map(serialize)
                .collect::<Result<_>>()?,
        ),
    ] {
        if !values.is_empty() {
            flags.push((key.to_string(), json!(values)));
        }
    }
    if gates.private_gate_reveal != defaults.private_gate_reveal {
        flags.push((
            "private-gate-reveal".to_string(),
            json!(
                clap::ValueEnum::to_possible_value(&gates.private_gate_reveal)
                    .expect("gate reveal policy has a clap value")
                    .get_name()
            ),
        ));
    }
    for (key, value, default) in [
        (
            "gate-execution-policy",
            &gates.gate_execution_policy,
            &defaults.gate_execution_policy,
        ),
        (
            "gate-environment-mode",
            &gates.gate_environment_mode,
            &defaults.gate_environment_mode,
        ),
    ] {
        if value != default {
            flags.push((key.to_string(), json!(value)));
        }
    }
    for (key, value, default) in [
        (
            "gate-timeout-seconds",
            gates.gate_timeout_seconds,
            defaults.gate_timeout_seconds,
        ),
        (
            "gate-heartbeat-interval-seconds",
            gates.gate_heartbeat_interval_seconds,
            defaults.gate_heartbeat_interval_seconds,
        ),
        (
            "gate-no-progress-timeout-seconds",
            gates.gate_no_progress_timeout_seconds,
            defaults.gate_no_progress_timeout_seconds,
        ),
    ] {
        if value != default {
            flags.push((key.to_string(), json!(value)));
        }
    }
    for (key, value, default) in [
        (
            "isolate-gate-home",
            gates.isolate_gate_home,
            defaults.isolate_gate_home,
        ),
        (
            "isolate-gate-xdg",
            gates.isolate_gate_xdg,
            defaults.isolate_gate_xdg,
        ),
    ] {
        if value != default {
            // `--isolate-gate-home false` takes an explicit value.
            flags.push((key.to_string(), json!(value.to_string())));
        }
    }
    for (key, enabled) in [
        ("rerun-completed-gates", gates.rerun_completed_gates),
        ("accept-inherited-failures", gates.accept_inherited_failures),
        ("no-component-lint-gate", gates.no_component_lint_gate),
        ("gate-shared-cargo-target", gates.gate_shared_cargo_target),
        (
            "no-gate-shared-cargo-target",
            gates.no_gate_shared_cargo_target,
        ),
    ] {
        if enabled {
            flags.push((key.to_string(), json!(true)));
        }
    }
    if let Some(runner) = &gates.gate_runner {
        flags.push(("gate-runner".to_string(), json!(runner)));
    }
    Ok(flags)
}

/// The gate flags' own defaults, read from the same clap definition.
fn default_gates() -> VerifyGateArgs {
    #[derive(clap::Parser)]
    struct Defaults {
        #[command(flatten)]
        gates: VerifyGateArgs,
    }
    <Defaults as clap::Parser>::parse_from(["gates"]).gates
}

fn serialize(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| Error::internal_json(error.to_string(), Some("cook-batch flag".into())))
}

fn placement_name(placement: Placement) -> String {
    clap::ValueEnum::to_possible_value(&placement)
        .expect("placement has a clap value")
        .get_name()
        .to_string()
}

fn apply_provider_profile(args: &mut AgentTaskFanoutCookBatchArgs) {
    let Some(profile) = selected_provider_profile(args.provider_profile.as_deref()) else {
        return;
    };
    if args.backend.is_none() {
        args.backend = profile.backend;
    }
    if args.selector.is_none() {
        args.selector = profile.selector;
    }
    if args.model.is_none() {
        args.model = profile.model;
    }
    if args.provider_config.is_none() {
        args.provider_config = profile.provider_config.map(|value| value.to_string());
    }
}

fn selected_provider_profile(name: Option<&str>) -> Option<AgentTaskProviderProfileDeclaration> {
    let name = name?.trim();
    AgentTaskProviderCatalog::discover()
        .providers()
        .iter()
        .flat_map(|provider| provider.cli.profiles.iter())
        .find(|profile| profile.name == name)
        .cloned()
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
        let mut segments = prefix.trim_end_matches('/').rsplit('/');
        let repo = segments.next().unwrap_or_default();
        let owner = segments.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() {
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
    let repo = registered_repo(&args.repo)?;
    let (repository, component) = super::run::cook_repository_names_for_selection(&repo, None)?;
    let path =
        super::run::cook_component_path_for_repository_name(&component)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "repo",
                format!("--repo `{}` has no registered checkout", args.repo),
                Some(args.repo.clone()),
                None,
            )
        })?;
    let resolution = resolve_default_branch(DefaultBranchRequest {
        explicit_base: args.base.as_deref(),
        explicit_from: None,
        workspace: None,
        component: Some(&path),
        destination: None,
        compatibility_fallback: None,
    })?;
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

/// A registered repository slug, from a slug or an exact registered primary
/// checkout path.
fn registered_repo(repo: &str) -> Result<String> {
    let path_like = Path::new(repo).is_absolute() || repo.contains(std::path::MAIN_SEPARATOR);
    if !path_like {
        return Ok(repo.to_string());
    }
    match homeboy::core::component::resolve_registered_primary_identity(repo)? {
        homeboy::core::component::RegisteredPrimaryPathResolution::Primary(id) => Ok(id),
        _ => Err(Error::validation_invalid_argument(
            "repo",
            "--repo must be a registered repo slug or an exact registered primary path",
            Some(repo.to_string()),
            None,
        )),
    }
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
        assert_eq!(cook["cook"]["gate-timeout-seconds"], 60);
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
        let mut visible = args.gates.clone();
        visible.snapshot_file_inputs().unwrap();
        homeboy::agents::agent_task_service::validate_reviewer_visible_gates(&visible.verify, None)
            .expect("a private gate is never published to reviewers");
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
