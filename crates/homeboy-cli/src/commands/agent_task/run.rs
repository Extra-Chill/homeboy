//! Durable run lifecycle handlers: cook, run-plan, run, run-next, submit,
//! resume, and retry.

use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use homeboy::agents::agent_task_service as agent_task_service_direct;
use homeboy::agents::agent_task_timeout::effective_provider_timeout_ms;
use homeboy::agents::agent_tasks::dispatch_service;
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::provider;
use homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::agents::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskPlan, SharedAgentTaskExecutor,
};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::core::command_invocation::CommandInvocation;
use homeboy::core::defaults;
use homeboy::core::worktree_providers::{
    plan_apply_enabled_worktree_provider_from_config,
    provision_apply_enabled_worktree_provider_from_config, WorktreeProviderCreateIntent,
    WorktreeProviderCreatePlan,
};

use super::super::agent_task_dispatch::DispatchArgs;
use super::super::CmdResult;
use super::args::{
    AgentTaskCookArgs, AgentTaskProviderEvidenceInput, CookContinueArgs, LifecycleReadArgs,
    PromotionProviderArgs, RetryArgs, RunArgs, RunNextArgs, RunPlanArgs, SubmitArgs,
};
use super::gate_contract::validate_gate_contracts;

const MAX_PROMOTION_PROVIDER_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PROVIDER_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;

/// Operator-facing durable identity block for a Cook that has just become
/// addressable.
///
/// A Cook's run id is the only handle an interrupted caller has. Everything
/// after durable submission — gate toolchain preflight, transport preparation,
/// Lab materialization, provider execution — can outlive a client timeout, so
/// the handle and its follow-up commands are assembled here and printed the
/// first time the controller reports identity (#10419, #9163).
pub(crate) fn durable_cook_identity_lines(cook_id: Option<&str>, run_id: &str) -> Vec<String> {
    let cook_suffix = cook_id
        .filter(|cook_id| *cook_id != run_id)
        .map(|cook_id| format!(" (cook `{cook_id}`)"))
        .unwrap_or_default();
    vec![
        format!("cook: durable run id `{run_id}`{cook_suffix} — persisted before materialization."),
        format!("cook: status -> homeboy --placement local agent-task status {run_id}"),
        format!("cook: logs   -> homeboy --placement local agent-task logs {run_id}"),
        format!("cook: cancel -> homeboy --placement local agent-task cancel {run_id}"),
    ]
}

/// Print the durable identity block unconditionally.
///
/// Deliberately not TTY-gated. The clients that most need this — agent/Discord
/// bridges, CI, and anything invoking Homeboy as a tool call — are exactly the
/// non-TTY callers that a TTY-gated status line silently skips, which is how an
/// interrupted Lab Cook ended up with no reported task identity at all (#10419).
pub(crate) fn announce_durable_cook_identity(cook_id: Option<&str>, run_id: &str) {
    for line in durable_cook_identity_lines(cook_id, run_id) {
        eprintln!("{line}");
    }
}

/// Operator-facing statement of the provider rotation a Cook will actually
/// perform.
///
/// A configured `agent_task.rotation` is only reachable if the execution budget
/// funds it, and nothing surfaced that mismatch — which is how a Cook could
/// carry a multi-provider rotation policy and still die terminally on the first
/// provider's recoverable failure while every other provider sat available
/// (#11082). State the effective behavior on every submission, including when
/// that behavior is "no rotation at all".
pub(crate) fn cook_rotation_disclosure(plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    let executions = budget.max_provider_executions;
    let entries = plan
        .options
        .rotation
        .as_ref()
        .map_or(0, |rotation| rotation.entries.len());
    let entries = u32::try_from(entries).unwrap_or(u32::MAX);
    // Only rotations that are both configured AND affordable will ever fire.
    let funded = entries
        .min(budget.max_provider_rotations)
        .min(executions.saturating_sub(1));
    let budget_facts = plan.metadata["cook_retry_policy"]
        .get("truncated")
        .and_then(|truncated| truncated.get("max_provider_rotations"))
        .and_then(Value::as_u64)
        .filter(|truncated| *truncated > 0)
        .map(|truncated| {
            let requested = plan.metadata["cook_retry_policy"]["requested"]
                ["max_provider_rotations"]
                .as_u64()
                .unwrap_or(u64::from(budget.max_provider_rotations) + truncated);
            format!(
                "; requested {requested} rotation(s), effective {}, truncated {truncated}",
                budget.max_provider_rotations
            )
        })
        .unwrap_or_default();
    if funded == 0 {
        let unreachable = if entries > 0 {
            format!(
                "; {entries} configured rotation provider(s) are unreachable at this budget \
                 (--max-provider-executions {executions}, --max-provider-rotations {})",
                budget.max_provider_rotations
            )
        } else {
            String::new()
        };
        format!(
            "cook: rotation: disabled ({executions} provider execution(s)){unreachable}{budget_facts}"
        )
    } else {
        format!(
            "cook: rotation: {funded} fallback provider(s), up to {executions} provider execution(s){budget_facts}"
        )
    }
}

/// Operator-facing statement that an attached local Cook shares its client's
/// lifetime.
///
/// `--detach-after-handoff` already documents the safe shape — with local
/// placement the Cook is re-executed in its own session, so it survives a client
/// that is interrupted or times out — but nothing said the default was the other
/// one. An attached local Cook runs its whole provider stack inside the calling
/// client's process tree, so a client that goes away takes the provider with it
/// and leaves the durable run reporting `queued` with zero attempts and nothing
/// executing it (#12570). This is diagnostics only: the default is unchanged and
/// is still frequently the right one, so state the consequence rather than
/// choosing differently.
pub(crate) fn cook_attached_local_placement_disclosure(
    provider_placement: Option<&str>,
    detach_after_handoff: bool,
) -> Option<String> {
    (provider_placement == Some("local") && !detach_after_handoff).then(|| {
        "cook: attached local placement — the provider runs in this client's process tree and will not survive it; pass --detach-after-handoff to re-execute the Cook in its own session".to_string()
    })
}

fn cook_resolved_policy_disclosure(max_attempts: u32, plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    format!(
        "cook: retry policy: 1 initial execution, {} same-provider remediation retry(ies), {} rotation(s), {} provider execution(s) maximum",
        budget.max_same_provider_retries,
        budget.max_provider_rotations,
        budget.max_provider_executions.max(max_attempts),
    )
}

/// Operator-facing statement of the wall-clock budget one provider execution
/// gets before it is cancelled at its deadline.
///
/// The budget was invisible: `--timeout-ms` documented only that it defaulted to
/// "Homeboy's provider timeout", so the only way to learn the number was to
/// spend a run discovering it — the deadline is named for the first time in a
/// `failure_classification: "timeout"` report, long after the sizing decision
/// that needed it (#12568). State it beside the retry budget, where the rest of
/// the execution budget is already disclosed.
fn cook_provider_timeout_disclosure(plan: &AgentTaskPlan) -> String {
    let limits = plan.tasks.first().map(|task| &task.limits);
    // Mirror the resolution the provider runner performs, including the
    // task-level value `AgentTaskPlan::canonicalize` would otherwise copy down
    // from plan options later.
    let timeout_ms = effective_provider_timeout_ms(
        limits
            .and_then(|limits| limits.timeout_ms)
            .or(plan.options.timeout_ms),
        limits.and_then(|limits| limits.max_runtime_ms),
    );
    format!(
        "cook: provider timeout: {}s per provider execution (override with --timeout-ms)",
        timeout_ms / 1_000
    )
}

/// Serialize a completed run aggregate and, when the run did not fully succeed,
/// surface a prominent top-level `failure_reasons` summary so the operator sees
/// the root cause (recipe validation, PHP fatal, provider registration, missing
/// path) without hand-digging the nested outcome JSON (#3806). The full nested
/// payload is preserved unchanged; this only ADDS the surfaced summary.
fn aggregate_value_with_failure_reasons(aggregate: &AgentTaskAggregate) -> Value {
    let aggregate = homeboy::agents::agent_task_artifacts::reviewer_facing_aggregate(aggregate);
    let mut value = serde_json::to_value(&aggregate).unwrap_or(Value::Null);
    let failure_reasons = super::status::failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        if let Value::Object(map) = &mut value {
            map.insert("failure_reasons".to_string(), Value::Array(failure_reasons));
        }
    }
    value
}

pub(crate) fn run_cook(mut args: AgentTaskCookArgs) -> CmdResult<Value> {
    args.gates.snapshot_file_inputs()?;
    let args = resolve_cook_destination(args)?;
    validate_cook_request(&args)?;
    run_cook_with_executor(
        args,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

/// Resolve the same pre-provisioning Cook inputs used by execution. This path
/// intentionally never calls `provision_cook_destination` or a durable service.
pub(crate) fn preview_cook(
    mut args: AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    args.gates.snapshot_file_inputs()?;
    // Source policy is a static validation and must apply to preview exactly as
    // it applies before an execution route can inspect the destination.
    validate_cook_request_with_provenance(&args, provenance)?;
    let (args, provision) = resolve_cook_preview_destination(args)?;
    let replay = cook_preview_replay_argv(&args);
    let placement = preview_placement_policy_with_admission(&replay.argv);
    if matches!(
        provision["action"].as_str(),
        Some("materialization_required" | "unresolved_provider")
    ) {
        return Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-cook-preview/v1",
                "mutates": false,
                "resolved": {
                    "repository": args.dispatch.repo,
                    "worktree": args.to_worktree,
                    "base": args.base,
                    "head": args.head,
                    "placement": placement,
                    "workspace": provision,
                },
                "replay_argv": replay.argv,
                "replay_requires": replay.requires,
            }),
            0,
        ));
    }
    let gate_workspace = args.dispatch.cwd.as_deref().map(Path::new).or_else(|| {
        args.to_worktree
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_dir())
    });
    let gate_contract_validation = validate_gate_contracts(
        args.gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .cloned(),
        gate_workspace,
        &crate::cli_runtime::current_augmented_command_contract(),
    )?;
    preflight_cook_provider_credentials(&args)?;

    // Preview binds evidence to the same resolved workspace, but only projects
    // its read-only paths. Cook alone performs the later secure copy.
    let mut compile_args = args.clone();
    compile_args.provider_evidence_inputs.clear();
    let evidence = if !args.provider_evidence_inputs.is_empty() {
        let workspace = provision["path"].as_str().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence cannot be projected until Cook has a resolved workspace path",
                None,
                None,
            )
        })?;
        let mut dispatch = dispatch_args_for_cook(&args);
        resolve_dispatch_prompt(&mut dispatch)?;
        validate_provider_evidence_inputs(
            &args.provider_evidence_inputs,
            dispatch.prompt.as_deref(),
        )?;
        compile_args.dispatch.prompt = dispatch.prompt;
        let evidence =
            projected_provider_evidence(&args.provider_evidence_inputs, Some(workspace))?;
        rewrite_provider_evidence_prompt(
            &mut compile_args.dispatch.prompt,
            &args.provider_evidence_inputs,
            Some(workspace),
        );
        Some(evidence)
    } else {
        None
    };
    let mut plan = compile_cook_plan(&compile_args, provision.clone())?;
    if let Some(evidence) = evidence {
        for task in &mut plan.tasks {
            if !task.executor.config.is_object() {
                task.executor.config = serde_json::json!({});
            }
            task.executor.config["evidence_inputs"] = serde_json::to_value(&evidence)
                .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        }
    }
    resolve_cook_execution_budget(&args, &mut plan)?;
    plan.metadata["gate_contract_validation"] = serde_json::to_value(gate_contract_validation)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;

    let executor = plan
        .tasks
        .first()
        .map(|task| serde_json::to_value(&task.executor).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "mutates": false,
            "resolved": {
                "repository": args.dispatch.repo,
                "worktree": args.to_worktree,
                "base": args.base,
                "head": args.head,
                "workspace": provision,
                "placement": placement,
                "provider": executor,
                "gates": {
                    "public": args.gates.verify.len(),
                    "private": args.gates.private_verify.len(),
                },
                "retry_budget": plan.metadata["cook_retry_policy"],
                "publication": {
                    "finalize": !args.no_finalize,
                    "draft": args.draft_pr,
                    "ai_tool": args.ai_tool,
                },
            },
            "replay_argv": replay.argv,
            "replay_requires": replay.requires,
        }),
        0,
    ))
}

const MAX_PREVIEW_REPLAY_ARGS: usize = 128;
const MAX_PREVIEW_REPLAY_BYTES: usize = 16 * 1024;

#[derive(Debug)]
struct PreviewReplayArgv {
    argv: Vec<String>,
    requires: Vec<String>,
}

fn cook_preview_replay_argv(args: &AgentTaskCookArgs) -> PreviewReplayArgv {
    let process_argv = std::env::args().collect::<Vec<_>>();
    if process_argv
        .windows(2)
        .any(|parts| parts == ["agent-task", "cook"])
        && process_argv.iter().any(|part| part == "--preview")
    {
        return redact_preview_replay_argv(
            std::iter::once("homeboy".to_string()).chain(
                process_argv
                    .into_iter()
                    .skip(1)
                    .filter(|part| part != "--preview"),
            ),
        );
    }

    // Unit callers do not have the original process argv. Keep their fallback
    // useful for embedding while the CLI path above preserves every supplied
    // advanced flag exactly.
    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    if let Some(prompt) = &args.dispatch.prompt {
        argv.extend(["--prompt".to_string(), prompt.clone()]);
    }
    if let Some(goal) = &args.goal {
        argv.extend(["--goal".to_string(), goal.clone()]);
    }
    if let Some(repo) = &args.dispatch.repo {
        argv.extend(["--repo".to_string(), repo.clone()]);
    }
    if let Some(task_url) = &args.dispatch.task_url {
        argv.extend(["--task-url".to_string(), task_url.clone()]);
    }
    if let Some(worktree) = &args.to_worktree {
        argv.extend(["--to-worktree".to_string(), worktree.clone()]);
    }
    for gate in &args.gates.verify {
        argv.extend(["--verify".to_string(), gate.clone()]);
    }
    argv.extend(["--base".to_string(), args.base.clone()]);
    if let Some(head) = &args.head {
        argv.extend(["--head".to_string(), head.clone()]);
    }
    if args.no_finalize {
        argv.push("--no-finalize".to_string());
    }
    if args.draft_pr {
        argv.push("--draft-pr".to_string());
    }
    redact_preview_replay_argv(argv)
}

fn redact_preview_replay_argv(argv: impl IntoIterator<Item = String>) -> PreviewReplayArgv {
    let (units, mut requires) = clap_replay_units(argv.into_iter().collect());
    let mut replay = Vec::new();
    let mut bytes = 0;
    for unit in units {
        let unit_bytes = unit.iter().map(String::len).sum::<usize>();
        if replay.len() + unit.len() > MAX_PREVIEW_REPLAY_ARGS
            || bytes + unit_bytes > MAX_PREVIEW_REPLAY_BYTES
        {
            requires.push("replay was truncated at the safety budget; re-add omitted complete flag/value units from the original command".to_string());
            break;
        }
        let (unit, requirement) = redact_replay_unit(unit);
        replay.extend(unit);
        bytes += unit_bytes;
        if let Some(requirement) = requirement {
            requires.push(requirement);
        }
    }
    PreviewReplayArgv {
        argv: replay,
        requires,
    }
}

fn clap_replay_units(argv: Vec<String>) -> (Vec<Vec<String>>, Vec<String>) {
    let value_flags = cook_value_flags();
    let mut units = Vec::new();
    let mut requires = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let value = &argv[index];
        if value == "--preview" {
            index += 1;
        } else if let Some((_flag, _)) = value.split_once('=') {
            units.push(vec![value.clone()]);
            index += 1;
        } else if value_flags.contains(value) {
            if let Some(next) = argv.get(index + 1).filter(|next| !next.starts_with("--")) {
                units.push(vec![value.clone(), next.clone()]);
                index += 2;
            } else {
                requires.push(format!("{value} was omitted because its value is absent; provide the original value before replaying"));
                index += 1;
            }
        } else {
            units.push(vec![value.clone()]);
            index += 1;
        }
    }
    (units, requires)
}

fn cook_value_flags() -> std::collections::BTreeSet<String> {
    let command = crate::cli_surface::Cli::command_with_scoped_lab_args();
    let mut flags = std::collections::BTreeSet::new();
    let agent_task = command
        .find_subcommand("agent-task")
        .expect("agent-task command exists in generated Clap metadata");
    let cook = agent_task
        .find_subcommand("cook")
        .expect("Cook command exists in generated Clap metadata");
    for command in [&command, agent_task, cook] {
        for arg in command
            .get_arguments()
            .filter(|arg| arg.get_action().takes_values())
        {
            if let Some(flag) = arg.get_long() {
                flags.insert(format!("--{flag}"));
            }
            if let Some(aliases) = arg.get_all_aliases() {
                flags.extend(aliases.into_iter().map(|alias| format!("--{alias}")));
            }
        }
    }
    flags
}

fn redact_replay_unit(unit: Vec<String>) -> (Vec<String>, Option<String>) {
    let flag = unit
        .first()
        .map(String::as_str)
        .unwrap_or_default()
        .split('=')
        .next()
        .unwrap_or_default();
    let sensitive = flag == "--private-verify"
        || flag == "--private-verify-file"
        || flag == "--gate-env"
        || flag == "--runner-env"
        || flag == "--lab-env-json"
        || matches!(
            flag,
            "--provider-config"
                | "--provider-command"
                | "--provider-argv"
                | "--client-context"
                | "--secret-env"
        );
    if !sensitive {
        return (unit, None);
    }
    let placeholder = if flag == "--gate-env" {
        "REDACTED=<redacted:--gate-env>".to_string()
    } else {
        format!("<redacted:{flag}>")
    };
    let replay = if unit.len() == 2 {
        vec![flag.to_string(), placeholder]
    } else {
        vec![format!("{flag}={placeholder}")]
    };
    (replay, Some(format!("{flag} was redacted; replace its parseable placeholder with the original value before replaying")))
}

fn preview_placement_policy() -> Value {
    let argv = std::env::args().collect::<Vec<_>>();
    preview_placement_policy_from_argv(&argv)
}

fn preview_placement_policy_with_admission(replay_args: &[String]) -> Value {
    let mut policy = preview_placement_policy();
    let placement = match policy["requested"].as_str() {
        Some("local") => crate::cli_surface::Placement::Local,
        Some("lab") => crate::cli_surface::Placement::Lab,
        Some("lab-or-local") => crate::cli_surface::Placement::LabOrLocal,
        _ => crate::cli_surface::Placement::Auto,
    };
    let runner = policy["runner"].as_str();
    let detach_after_handoff = policy["detach_after_handoff"].as_bool().unwrap_or(false);
    let admission = match crate::commands::resources::run_preflight() {
        Ok((resources, _)) => {
            let readiness = (!placement.is_explicit_local_override())
                .then(crate::runner::lab_runner_readiness)
                .transpose()
                .ok()
                .flatten();
            crate::commands::utils::resource_policy::cook_preview_placement_admission(
                crate::commands::utils::resource_policy::HotCommand {
                    label: "agent-task cook/run-plan/retry --run",
                    lab_offload_supported: true,
                    lab_offload_unsupported_reason: None,
                    allows_warm_runner_coordination: true,
                    offload_only_when_hot: false,
                },
                &resources,
                placement,
                runner,
                detach_after_handoff,
                readiness.as_ref(),
                replay_args,
            )
        }
        Err(error) => crate::commands::utils::resource_policy::CookPreviewPlacementAdmission {
            schema: "homeboy/cook-preview-placement-admission/v1",
            state: crate::commands::utils::resource_policy::CookPreviewPlacementAdmissionState::Indeterminate,
            revalidate_before_execution: true,
            blockers: vec![
                crate::commands::utils::resource_policy::CookPreviewPlacementBlocker {
                    id: "controller_resource_snapshot_unavailable".to_string(),
                    detail: error.message,
                },
            ],
            deferred_to: Some("controller_resource_snapshot".to_string()),
            recovery: None,
        },
    };
    policy["admission"] =
        serde_json::to_value(admission).expect("Cook preview placement admission serializes");
    policy
}

fn preview_placement_policy_from_argv(argv: &[String]) -> Value {
    let argv = crate::command_capability::homeboy_owned_args(argv);
    let placement = argv
        .iter()
        .enumerate()
        .find_map(|(index, value)| {
            value
                .strip_prefix("--placement=")
                .map(str::to_string)
                .or_else(|| {
                    (value == "--placement")
                        .then(|| argv.get(index + 1).cloned())
                        .flatten()
                })
        })
        .unwrap_or_else(|| "auto".to_string());
    let runner = argv.iter().enumerate().find_map(|(index, value)| {
        value
            .strip_prefix("--runner=")
            .map(str::to_string)
            .or_else(|| {
                (value == "--runner")
                    .then(|| argv.get(index + 1).cloned())
                    .flatten()
            })
    });
    serde_json::json!({
        "requested": placement,
        "runner": runner,
        "detach_after_handoff": argv.iter().any(|value| value == "--detach-after-handoff"),
        "route_executed": false,
    })
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    fn cook(argv: &[&str]) -> AgentTaskCookArgs {
        let cli = Cli::try_parse_from(argv).expect("parse Cook preview");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("Cook command");
        };
        *cook
    }

    #[test]
    fn preview_reports_destination_blockers_without_creating_homeboy_state() {
        crate::test_support::with_isolated_home(|home| {
            let before = std::fs::read_dir(home).expect("read isolated home").count();
            let error = preview_cook(
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--prompt",
                    "implement the issue",
                    "--backend",
                    "fixture",
                    "--no-finalize",
                ]),
                None,
            )
            .expect_err("missing destination must block preview");

            assert_eq!(
                error.code,
                homeboy::core::ErrorCode::ValidationMissingArgument
            );
            assert_eq!(
                error.details["args"],
                serde_json::json!(["--repo <repo> is required when --to-worktree is omitted"])
            );
            assert_eq!(
                std::fs::read_dir(home).expect("read isolated home").count(),
                before,
                "preview must not create Homeboy state"
            );
        });
    }

    #[test]
    fn preview_replay_argv_is_executable_cook_argv_without_preview() {
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--preview",
            "--prompt",
            "implement the issue",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/12478",
            "--verify",
            "cargo test -p homeboy-cli",
        ]);
        let resolved = resolve_cook_destination(args).expect("resolve issue destination");
        let replay = cook_preview_replay_argv(&resolved);
        assert!(
            !replay.argv.iter().any(|part| part == "--preview"),
            "{replay:?}"
        );
        Cli::try_parse_from(&replay.argv).expect("replay argv parses as Cook");
    }

    #[cfg(unix)]
    #[test]
    fn preview_plans_an_absent_issue_workspace_without_ensuring_it() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|home| {
            let temp = tempfile::tempdir().expect("tempdir");
            let marker = temp.path().join("ensure-called");
            let provider = temp.path().join("provider.sh");
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nresolve) printf '%s\\n' '{{\"worktrees\":[]}}' ;;\nplan) printf '%s\\n' \"{{\\\"worktrees\\\":[{{\\\"handle\\\":\\\"$2\\\",\\\"path\\\":\\\"/provider/planned/$2\\\",\\\"branch\\\":\\\"$5\\\",\\\"safety\\\":{{\\\"dirty\\\":false,\\\"unpushed\\\":false,\\\"primary\\\":false}}}}]}}\" ;;\nensure) touch '{}' ;;\nesac\n",
                    marker.display(),
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("make provider executable");

            let mut config = homeboy::core::defaults::load_config();
            config.worktree_providers.insert(
                "fixture".to_string(),
                homeboy::core::defaults::WorktreeProviderConfig {
                    enabled: true,
                    kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                    apply_enabled: true,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: homeboy::core::defaults::WorktreeProviderCommands {
                        resolve: Some(vec![
                            provider.display().to_string(),
                            "resolve".to_string(),
                            "{handle}".to_string(),
                        ]),
                        plan: Some(vec![
                            provider.display().to_string(),
                            "plan".to_string(),
                            "{handle}".to_string(),
                            "{repo}".to_string(),
                            "{base}".to_string(),
                            "{head}".to_string(),
                            "{task_url}".to_string(),
                        ]),
                        ensure: Some(vec![provider.display().to_string(), "ensure".to_string()]),
                        ..Default::default()
                    },
                    list_result_mapping: Some(
                        homeboy::core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            homeboy::core::defaults::save_config(&config).expect("save provider config");
            let before = std::fs::read_dir(home).expect("read isolated home").count();

            let (preview, exit_code) = preview_cook(
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "implement the issue",
                    "--repo",
                    "fixture",
                    "--task-url",
                    "https://example.test/owner/repo/issues/12890",
                    "--no-finalize",
                ]),
                None,
            )
            .expect("plan absent issue workspace");

            assert_eq!(exit_code, 0);
            assert_eq!(preview["resolved"]["workspace"]["action"], "planned_create");
            assert_eq!(preview["resolved"]["workspace"]["provider_id"], "fixture");
            assert_eq!(
                preview["resolved"]["workspace"]["branch"],
                "fix/issue-12890-repo"
            );
            assert_eq!(
                preview["resolved"]["workspace"]["path"],
                "/provider/planned/fixture@fix-issue-12890-repo"
            );
            assert_eq!(preview["resolved"]["workspace"]["intent"]["base"], "main");
            assert_eq!(
                preview["resolved"]["workspace"]["intent"]["head"],
                "fix/issue-12890-repo"
            );
            assert!(!marker.exists(), "preview must not invoke ensure");
            assert_eq!(
                std::fs::read_dir(home).expect("read isolated home").count(),
                before
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn preview_reports_provider_reuse_and_unresolved_planning_without_ensuring() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let marker = temp.path().join("ensure-called");
            let provider = temp.path().join("provider.sh");
            let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .expect("workspace root");
            let branch = std::process::Command::new("git")
                .args([
                    "-C",
                    workspace.to_str().expect("UTF-8 workspace"),
                    "branch",
                    "--show-current",
                ])
                .output()
                .expect("read workspace branch");
            let branch = String::from_utf8(branch.stdout)
                .expect("UTF-8 branch")
                .trim()
                .to_string();
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nresolve) printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-issue-12890-homeboy\",\"path\":\"{}\",\"branch\":\"{}\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}' ;;\nmissing) printf '%s\\n' '{{\"worktrees\":[]}}' ;;\nensure) touch '{}' ;;\nesac\n",
                    workspace.display(),
                    branch,
                    marker.display(),
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("make provider executable");

            let mut config = homeboy::core::defaults::load_config();
            config.worktree_providers.insert(
                "fixture".to_string(),
                homeboy::core::defaults::WorktreeProviderConfig {
                    enabled: true,
                    kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                    apply_enabled: true,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: homeboy::core::defaults::WorktreeProviderCommands {
                        resolve: Some(vec![
                            provider.display().to_string(),
                            "resolve".to_string(),
                            "{handle}".to_string(),
                        ]),
                        ensure: Some(vec![provider.display().to_string(), "ensure".to_string()]),
                        ..Default::default()
                    },
                    list_result_mapping: Some(
                        homeboy::core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            homeboy::core::defaults::save_config(&config).expect("save provider config");

            let args = || {
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "implement the issue",
                    "--repo",
                    "homeboy",
                    "--task-url",
                    "https://github.com/Extra-Chill/homeboy/issues/12890",
                    "--no-finalize",
                ])
            };
            let (reused, exit_code) = preview_cook(args(), None).expect("plan reused workspace");
            assert_eq!(exit_code, 0);
            assert_eq!(
                reused["resolved"]["workspace"]["action"], "planned_reuse",
                "{reused}"
            );
            assert_eq!(reused["resolved"]["workspace"]["provider_id"], "fixture");
            assert_eq!(
                reused["resolved"]["workspace"]["path"],
                workspace.display().to_string()
            );

            config
                .worktree_providers
                .get_mut("fixture")
                .unwrap()
                .commands
                .resolve = Some(vec![provider.display().to_string(), "missing".to_string()]);
            homeboy::core::defaults::save_config(&config).expect("save unresolved config");
            let (unresolved, exit_code) =
                preview_cook(args(), None).expect("report unresolved provider");
            assert_eq!(exit_code, 0);
            assert_eq!(
                unresolved["resolved"]["workspace"]["action"],
                "unresolved_provider"
            );
            assert_eq!(
                unresolved["resolved"]["workspace"]["provider_id"],
                "fixture"
            );
            assert!(!marker.exists(), "preview must never invoke ensure");
        });
    }

    #[test]
    fn preview_applies_argument_provenance_before_destination_resolution() {
        let mut provenance = crate::cli_surface::CommandArgumentProvenance::default();
        provenance.set(
            "no_finalize",
            crate::cli_surface::ArgumentSource::Configuration,
        );
        let error = preview_cook(
            cook(&[
                "homeboy",
                "agent-task",
                "cook",
                "--preview",
                "--prompt",
                "implement the issue",
                "--to-worktree",
                "missing@worktree",
                "--no-finalize",
            ]),
            Some(&provenance),
        )
        .expect_err("preview must enforce no-finalize provenance");
        assert!(
            error.message.contains("explicitly authorized"),
            "{}",
            error.message
        );
    }

    #[test]
    fn preview_replay_redacts_opaque_provider_values_and_arbitrary_credential_names() {
        let replay = redact_preview_replay_argv(std::iter::once("homeboy".to_string()).chain([
            "agent-task".to_string(),
            "cook".to_string(),
            "--gate-env".to_string(),
            "TOKEN=secret-value".to_string(),
            "--provider-config".to_string(),
            r#"{"token":"secret-value"}"#.to_string(),
            "--private-verify".to_string(),
            "printf secret-value".to_string(),
            "--provider-argv".to_string(),
            "credential-without-a-secret-name".to_string(),
            "--runner-env=TOKEN=secret-value".to_string(),
        ]));
        assert!(
            !replay.argv.join(" ").contains("secret-value"),
            "{replay:?}"
        );
        assert_eq!(replay.argv[4], "REDACTED=<redacted:--gate-env>");
        assert_eq!(replay.argv[6], "<redacted:--provider-config>");
        assert_eq!(replay.argv[7], "--private-verify");
        assert_eq!(replay.argv[8], "<redacted:--private-verify>");
        assert_eq!(replay.argv[10], "<redacted:--provider-argv>");
        assert_eq!(replay.argv[11], "--runner-env=<redacted:--runner-env>");
        assert_eq!(replay.requires.len(), 5);
        Cli::try_parse_from(&replay.argv).expect("redacted replay remains parseable");
    }

    #[test]
    fn preview_replay_truncates_only_at_parseable_clap_unit_boundaries() {
        let argv = std::iter::once("homeboy".to_string())
            .chain(["agent-task".to_string(), "cook".to_string()])
            .chain(
                (0..MAX_PREVIEW_REPLAY_ARGS)
                    .flat_map(|index| ["--verify".to_string(), format!("true-{index}")]),
            )
            .collect::<Vec<_>>();
        let replay = redact_preview_replay_argv(argv);
        assert!(replay.argv.len() < MAX_PREVIEW_REPLAY_ARGS);
        assert!(replay
            .requires
            .iter()
            .any(|item| item.contains("safety budget")));
        for boundary in (3..=replay.argv.len()).step_by(2) {
            Cli::try_parse_from(&replay.argv[..boundary])
                .unwrap_or_else(|error| panic!("boundary {boundary} is not parseable: {error}"));
        }
    }

    #[test]
    fn every_generated_cook_value_option_is_atomic_and_never_dangles() {
        for flag in cook_value_flags() {
            let (_, missing_requires) = clap_replay_units(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                flag.clone(),
            ]);
            assert!(
                missing_requires
                    .iter()
                    .any(|requirement| requirement.starts_with(&flag)),
                "missing value-taking flag {flag} was not withheld atomically"
            );

            let (units, requires) = clap_replay_units(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                flag.clone(),
                "placeholder".to_string(),
            ]);
            assert!(
                requires.is_empty(),
                "complete {flag} unexpectedly needs input"
            );
            assert!(
                units
                    .iter()
                    .any(|unit| unit == &[flag.clone(), "placeholder".to_string()]),
                "value-taking flag {flag} was not retained as one unit"
            );
        }
    }

    #[test]
    fn root_value_options_and_aliases_are_atomic_in_separated_and_equals_forms() {
        let root_values = [
            ("--output", "preview.json"),
            ("--notification-transport", "webhook"),
            ("--notification-route", "https://example.test/hook"),
            ("--placement", "auto"),
            ("--artifact-root", "artifacts"),
            ("--runner", "runner-1"),
            ("--runner-env", "TOKEN=value"),
            ("--runner-secret-env", "TOKEN"),
            ("--lab-env-json", "{}"),
            ("--runner-workspace-root", "workspace"),
        ];
        let value_flags = cook_value_flags();
        for (flag, value) in root_values {
            assert!(value_flags.contains(flag), "missing root value flag {flag}");
            let (separated, requires) = clap_replay_units(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                flag.to_string(),
                value.to_string(),
            ]);
            assert!(requires.is_empty(), "{flag} separated form needs input");
            assert!(
                separated
                    .iter()
                    .any(|unit| unit == &[flag.to_string(), value.to_string()]),
                "{flag} separated form was not atomic"
            );

            let (equals, requires) = clap_replay_units(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                format!("{flag}={value}"),
            ]);
            assert!(requires.is_empty(), "{flag} equals form needs input");
            assert!(
                equals
                    .iter()
                    .any(|unit| unit == &[format!("{flag}={value}")]),
                "{flag} equals form was not atomic"
            );
        }
    }

    #[test]
    fn root_value_units_are_never_split_at_any_replay_budget_boundary() {
        for flag in [
            "--output",
            "--notification-route",
            "--runner-env",
            "--lab-env-json",
        ] {
            let argv = std::iter::once("homeboy".to_string())
                .chain(["agent-task".to_string(), "cook".to_string()])
                .chain(
                    (0..MAX_PREVIEW_REPLAY_ARGS)
                        .flat_map(|index| [flag.to_string(), format!("value-{index}")]),
                )
                .collect::<Vec<_>>();
            let replay = redact_preview_replay_argv(argv);
            assert_eq!(replay.argv.len() % 2, 1, "{flag} was split at the budget");
            assert!(replay
                .requires
                .iter()
                .any(|item| item.contains("safety budget")));
        }
    }

    #[test]
    fn valid_root_value_forms_parse_before_and_after_cook() {
        for (flag, value) in [
            ("--output", "preview.json"),
            ("--placement", "local"),
            ("--artifact-root", "artifacts"),
            ("--runner-env", "TOKEN=value"),
            ("--runner-secret-env", "TOKEN"),
            ("--lab-env-json", "{}"),
            ("--runner-workspace-root", "workspace"),
        ] {
            Cli::try_parse_from(["homeboy", flag, value, "agent-task", "cook"])
                .unwrap_or_else(|error| panic!("separated {flag} must parse: {error}"));
            let equals = format!("{flag}={value}");
            Cli::try_parse_from(["homeboy", "agent-task", "cook", &equals])
                .unwrap_or_else(|error| panic!("equals {flag} must parse: {error}"));
        }
        for args in [
            vec![
                "homeboy",
                "--notification-transport",
                "webhook",
                "--notification-route",
                "https://example.test/hook",
                "agent-task",
                "cook",
            ],
            vec![
                "homeboy",
                "agent-task",
                "cook",
                "--notification-transport=webhook",
                "--notification-route=https://example.test/hook",
            ],
        ] {
            Cli::try_parse_from(args)
                .unwrap_or_else(|error| panic!("notification pair must parse: {error}"));
        }
    }

    #[test]
    fn preview_reports_requested_placement_without_executing_a_route() {
        let policy = preview_placement_policy_from_argv(&[
            "homeboy".to_string(),
            "--placement=lab-or-local".to_string(),
            "--detach-after-handoff".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--".to_string(),
            "--placement=local".to_string(),
            "--runner=forwarded".to_string(),
        ]);
        assert_eq!(policy["requested"], "lab-or-local");
        assert_eq!(policy["runner"], Value::Null);
        assert_eq!(policy["detach_after_handoff"], true);
        assert_eq!(policy["route_executed"], false);
    }

    #[test]
    fn compiled_plan_preview_emits_placement_admission_schema() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .expect("workspace root")
                .display()
                .to_string();
            let cli = Cli::try_parse_from([
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--preview".to_string(),
                "--backend".to_string(),
                "fixture".to_string(),
                "--prompt".to_string(),
                "inspect the task".to_string(),
                "--to-worktree".to_string(),
                workspace,
                "--no-finalize".to_string(),
                "--verify".to_string(),
                "true".to_string(),
            ])
            .expect("parse preview");
            let Commands::AgentTask(agent_task) = cli.command else {
                panic!("agent-task command");
            };
            let super::super::AgentTaskCommand::Cook(args) = agent_task.command else {
                panic!("Cook command");
            };
            let (preview, exit_code) = preview_cook(*args, None).expect("compile preview");

            assert_eq!(exit_code, 0);
            assert_eq!(preview["schema"], "homeboy/agent-task-cook-preview/v1");
            assert_eq!(preview["mutates"], false);
            assert!(preview["resolved"]["provider"].is_object());
            assert_eq!(
                preview["resolved"]["placement"]["admission"]["schema"],
                "homeboy/cook-preview-placement-admission/v1"
            );
            assert!(matches!(
                preview["resolved"]["placement"]["admission"]["state"].as_str(),
                Some("admissible" | "blocked" | "indeterminate")
            ));
            assert_eq!(
                preview["resolved"]["placement"]["admission"]["revalidate_before_execution"],
                true
            );
        });
    }

    #[test]
    fn read_only_evidence_projection_uses_the_resolved_workspace_path() {
        let workspace = tempfile::tempdir().expect("workspace");
        let source = tempfile::NamedTempFile::new().expect("evidence");
        std::fs::write(source.path(), "evidence").expect("write evidence");
        let evidence = vec![AgentTaskProviderEvidenceInput {
            id: "issue".to_string(),
            source: source.path().display().to_string(),
        }];
        validate_provider_evidence_inputs(&evidence, Some("Read the evidence."))
            .expect("validate evidence");
        let projected = projected_provider_evidence(&evidence, workspace.path().to_str())
            .expect("project read-only evidence path");
        assert!(projected[0]["path"]
            .as_str()
            .expect("projection path")
            .starts_with(workspace.path().to_str().expect("workspace path")));
        assert!(projected[0]["read_only"].as_bool().expect("read-only flag"));

        let bounded = redact_preview_replay_argv(
            (0..MAX_PREVIEW_REPLAY_ARGS + 1).map(|index| format!("arg-{index}")),
        );
        assert_eq!(bounded.argv.len(), MAX_PREVIEW_REPLAY_ARGS);
        assert!(bounded
            .requires
            .iter()
            .any(|item| item.contains("safety budget")));
    }

    #[test]
    fn compile_cook_plan_returns_a_typed_evidence_blocker_without_a_workspace() {
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "read the evidence",
            "--to-worktree",
            "missing@worktree",
            "--no-finalize",
            "--provider-evidence",
            r#"{"id":"issue","source":"/tmp/issue.md"}"#,
        ]);
        let error = compile_cook_plan(&args, serde_json::json!({ "action": "lookup_pending" }))
            .expect_err("evidence without a workspace must not panic");
        assert_eq!(
            error.code,
            homeboy::core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(error.details["field"], "provider-evidence");
    }

    #[cfg(unix)]
    #[test]
    fn preview_reports_registered_remote_provider_workspace_without_materializing_it() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let marker = temp.path().join("provider-ran");
            let provider = temp.path().join("provider.sh");
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nensure) touch '{}' ;;\nsentinel@missing) printf '%s\\n' '{{\"worktrees\":[]}}' ;;\nsentinel@malformed) printf '{{' ;;\n*) printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"sentinel@remote\",\"path\":\"remote://fixture/sentinel\",\"branch\":\"remote\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}' ;;\nesac\n",
                    marker.display()
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("make provider executable");

            let mut config = homeboy::core::defaults::load_config();
            config.worktree_providers.insert(
                "sentinel".to_string(),
                homeboy::core::defaults::WorktreeProviderConfig {
                    enabled: true,
                    kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                    apply_enabled: true,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: homeboy::core::defaults::WorktreeProviderCommands {
                        resolve: Some(vec![provider.display().to_string(), "{handle}".to_string()]),
                        ensure: Some(vec![provider.display().to_string(), "ensure".to_string()]),
                        ..Default::default()
                    },
                    list_result_mapping: Some(
                        homeboy::core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            homeboy::core::defaults::save_config(&config).expect("save provider config");

            let (preview, exit_code) = preview_cook(
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "implement the issue",
                    "--to-worktree",
                    "sentinel@remote",
                    "--no-finalize",
                ]),
                None,
            )
            .expect("registered remote workspace returns a preview requirement");
            assert_eq!(exit_code, 0);
            assert_eq!(preview["schema"], "homeboy/agent-task-cook-preview/v1");
            assert_eq!(preview["mutates"], false);
            assert_eq!(
                preview["resolved"]["placement"]["admission"]["schema"],
                "homeboy/cook-preview-placement-admission/v1"
            );
            assert!(matches!(
                preview["resolved"]["placement"]["admission"]["state"].as_str(),
                Some("admissible" | "blocked" | "indeterminate")
            ));
            assert_eq!(
                preview["resolved"]["placement"]["admission"]["revalidate_before_execution"],
                true
            );
            assert_eq!(
                preview["resolved"]["workspace"]["action"],
                "materialization_required"
            );
            assert_eq!(preview["resolved"]["workspace"]["provider_id"], "sentinel");
            assert!(
                !marker.exists(),
                "preview must not execute provider materialization"
            );
            let missing = preview_cook(
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "implement the issue",
                    "--to-worktree",
                    "sentinel@missing",
                    "--no-finalize",
                ]),
                None,
            )
            .expect_err("missing handle is not a materialization requirement");
            assert_eq!(missing.details["worktree_provider_lookup"], "not_found");
            let malformed = preview_cook(
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "implement the issue",
                    "--to-worktree",
                    "sentinel@malformed",
                    "--no-finalize",
                ]),
                None,
            )
            .expect_err("malformed provider result must fail closed");
            assert_eq!(
                malformed.details["worktree_provider_call_classification"],
                "malformed"
            );
            assert!(
                !marker.exists(),
                "preview must not materialize missing or malformed destinations"
            );
        });
    }

    #[test]
    fn preview_rejects_an_existing_unsafe_path_without_changing_it() {
        crate::test_support::with_isolated_home(|_| {
            let path = tempfile::tempdir().expect("unsafe path");
            let sentinel = path.path().join("unchanged");
            std::fs::write(&sentinel, "keep").expect("write sentinel");
            let error = preview_cook(
                cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--preview",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "implement the issue",
                    "--to-worktree",
                    path.path().to_str().expect("UTF-8 path"),
                    "--no-finalize",
                ]),
                None,
            )
            .expect_err("non-worktree path must fail static safety checks");
            assert_eq!(
                error.code,
                homeboy::core::ErrorCode::ValidationInvalidArgument
            );
            assert_eq!(
                std::fs::read_to_string(&sentinel).expect("read sentinel"),
                "keep"
            );
        });
    }
}

/// Resume a Cook from its immutable recipe rather than asking the operator to
/// replay prompt, provider, gate, workspace, or disclosure arguments.
pub(crate) fn continue_cook(args: CookContinueArgs) -> CmdResult<Value> {
    continue_cook_with(
        args,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )
}

pub(crate) fn continue_cook_with<F>(
    args: CookContinueArgs,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    continue_cook_with_queued_execution(args, executor, reconstruct_dispatcher, false)
}

/// `retry --run` owns a fresh queued replacement attempt. It may dispatch that
/// attempt through the immutable Cook recipe; ordinary `cook-continue` remains
/// observation-only until the attempt becomes terminal.
fn continue_cook_with_queued_execution<F>(
    args: CookContinueArgs,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
    execute_queued_attempt: bool,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    let recipe =
        agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        })?;
    let run_id = agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id)?;
    let record = agent_task_service::reconcile_recipe_attempt_for_continuation(&recipe, &run_id)?;
    if execute_queued_attempt && record.state == agent_task_lifecycle::AgentTaskRunState::Queued {
        return dispatch_queued_cook_retry(
            &recipe,
            &run_id,
            args.full,
            executor,
            reconstruct_dispatcher,
        );
    }
    if !record.state.is_terminal() {
        return Ok((cook_continuation_status(&recipe.cook_id, &record), 0));
    }

    if matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
    ) {
        // Ordinary continuation consumes only the pending entry scheduled by
        // authoritative reconciliation. Failed and completed claims require an
        // explicit rearm and can never be silently replayed.
        let claim = if args.rearm {
            agent_task_service::claim_continuation_for_recovery(&recipe.cook_id, &run_id)?
        } else {
            agent_task_service::claim_continuation_for(&recipe.cook_id, &run_id)?
        };
        let Some(claim) = claim else {
            return Ok((
                cook_terminal_continuation_status(
                    &recipe.cook_id,
                    &run_id,
                    &format!("{:?}", record.state),
                    agent_task_service::continuation_state(&recipe.cook_id, &run_id)?,
                ),
                0,
            ));
        };
        let mut result = None;
        let historical_terminal =
            agent_task_service_direct::historical_terminal_continuation_is_eligible(
                &recipe,
                record.state,
            );
        let dispatcher = reconstruct_dispatcher;
        let executor = executor.clone();
        let execute = |options| {
            agent_task_service::authorize_cook_continue_route_with_artifact(
                &options,
                args.artifact_id.as_deref(),
            )?;
            let cook = if historical_terminal {
                agent_task_service::run_terminal_cook_continuation(options, executor.clone())?
            } else {
                agent_task_service::run_cook(agent_task_service::CookContext::new(
                    options,
                    executor.clone(),
                ))?
            };
            let exit_code = cook.exit_code;
            result = Some(cook.value);
            Ok(exit_code)
        };
        let exit_code = if historical_terminal {
            agent_task_service::consume_claimed_terminal_with_dispatcher(
                claim, dispatcher, execute,
            )?
        } else {
            agent_task_service::consume_claimed_with_dispatcher(claim, dispatcher, execute)?
        };
        let value = cook_report_with_continuation(
            serde_json::to_value(result.ok_or_else(|| {
                homeboy::core::Error::internal_unexpected(
                    "claimed Cook continuation returned no result",
                )
            })?)
            .unwrap_or(Value::Null),
        );
        return Ok((
            super::status::compact_cook_report(value, args.full),
            exit_code,
        ));
    }

    let dispatcher = reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cook_or_attempt_id",
                "selected attempt is absent from its durable Cook recipe",
                Some(run_id.clone()),
                None,
            )
        })?;
    let terminal_review_form_continuation =
        agent_task_service::terminal_review_form_continuation_is_eligible(&attempt.plan, &record)?;
    let mut options = if terminal_review_form_continuation {
        agent_task_service::reconstruct_adoption_options_with_dispatcher(&recipe, dispatcher)?
    } else {
        agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)?
    };
    options.initial_run_id = attempt.run_id.clone();
    options.initial_plan = attempt.plan.clone();
    agent_task_service::authorize_cook_continue_route_with_artifact(
        &options,
        args.artifact_id.as_deref(),
    )?;
    let result = if terminal_review_form_continuation {
        agent_task_service::run_terminal_cook_continuation(options, executor)?
    } else {
        agent_task_service::run_cook(agent_task_service::CookContext::new(options, executor))?
    };
    let value =
        cook_report_with_continuation(serde_json::to_value(result.value).unwrap_or(Value::Null));
    Ok((
        super::status::compact_cook_report(value, args.full),
        result.exit_code,
    ))
}

#[cfg(test)]
pub(crate) fn consume_queued_cook_retry_with<F>(
    args: CookContinueArgs,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    continue_cook_with_queued_execution(args, executor, reconstruct_dispatcher, true)
}

/// A retry reservation creates a queued record before its external dispatch.
/// Claim that effect separately so competing `retry --run` consumers converge
/// on one dispatcher invocation.
fn dispatch_queued_cook_retry<F>(
    recipe: &homeboy::agents::agent_task_service::AgentTaskCookRecipe,
    run_id: &str,
    full: bool,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    let operation_key = format!("retry-run:{run_id}");
    match agent_task_lifecycle::claim_cook_operation(
        run_id,
        &operation_key,
        Duration::from_secs(30),
    )? {
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_)
        | agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            let record = agent_task_lifecycle::status(run_id)?;
            Ok((cook_continuation_status(&recipe.cook_id, &record), 0))
        }
        agent_task_lifecycle::ClaimOutcome::Acquired => {
            let dispatched: CmdResult<Value> = (|| {
                let dispatcher =
                    reconstruct_dispatcher(&recipe.promotion_transport["attempt_dispatch"])?;
                let attempt = recipe
                    .attempts
                    .iter()
                    .find(|attempt| attempt.run_id == run_id)
                    .ok_or_else(|| {
                        homeboy::core::Error::validation_invalid_argument(
                            "cook_or_attempt_id",
                            "selected attempt is absent from its durable Cook recipe",
                            Some(run_id.to_string()),
                            None,
                        )
                    })?;
                let mut options =
                    agent_task_service::reconstruct_options_with_dispatcher(recipe, dispatcher)?;
                options.initial_run_id = attempt.run_id.clone();
                options.initial_plan = attempt.plan.clone();
                agent_task_service::authorize_cook_continue_route(&options)?;
                let result = agent_task_service::run_cook(agent_task_service::CookContext::new(
                    options, executor,
                ))?;
                let value = cook_report_with_continuation(
                    serde_json::to_value(result.value).unwrap_or(Value::Null),
                );
                Ok((
                    super::status::compact_cook_report(value, full),
                    result.exit_code,
                ))
            })();
            match dispatched {
                Ok((value, exit_code)) => {
                    agent_task_lifecycle::complete_cook_operation(
                        run_id,
                        &operation_key,
                        serde_json::json!({ "exit_code": exit_code }),
                    )?;
                    Ok((value, exit_code))
                }
                Err(error) => {
                    agent_task_lifecycle::fail_cook_operation(
                        run_id,
                        &operation_key,
                        serde_json::json!({ "error": error.message.clone() }),
                    )?;
                    Err(error)
                }
            }
        }
    }
}

/// Probe the continuation admission boundary without claiming a continuation or
/// calling any execution, transport, or finalization API.
pub(crate) fn preflight_continue_cook(args: CookContinueArgs) -> CmdResult<Value> {
    let mut phases = Vec::new();
    let mut selected_run_id = None;
    let mut candidate_fingerprint = Value::Null;
    let recipe =
        match agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        }) {
            Ok(recipe) => {
                phases.push(
                    serde_json::json!({ "phase": "recipe", "status": "passed", "reason": "ok" }),
                );
                recipe
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(None, Value::Null, phases, "recipe", &error),
                    1,
                ))
            }
        };
    let run_id =
        match agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id) {
            Ok(run_id) => {
                selected_run_id = Some(run_id.clone());
                phases.push(
                    serde_json::json!({ "phase": "selection", "status": "passed", "reason": "ok" }),
                );
                run_id
            }
            Err(error) => {
                return Ok((
                    cook_continuation_preflight_report(
                        selected_run_id,
                        Value::Null,
                        phases,
                        "selection",
                        &error,
                    ),
                    1,
                ))
            }
        };
    let record = match agent_task_service::reconcile_recipe_attempt_for_continuation(
        &recipe, &run_id,
    ) {
        Ok(record) => {
            candidate_fingerprint = record
                .metadata
                .pointer("/latest_promotion/provenance/candidate")
                .cloned()
                .unwrap_or(Value::Null);
            phases.push(serde_json::json!({ "phase": "lifecycle", "status": "passed", "reason": "ok", "state": format!("{:?}", record.state) }));
            record
        }
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    candidate_fingerprint,
                    phases,
                    "lifecycle",
                    &error,
                ),
                1,
            ))
        }
    };
    if !record.state.is_terminal() {
        let error = homeboy::core::Error::validation_invalid_argument(
            "cook_or_attempt_id",
            "selected Cook attempt is not terminal and cannot be admitted to dispatch",
            Some(run_id),
            None,
        );
        return Ok((
            cook_continuation_preflight_report(
                selected_run_id,
                candidate_fingerprint,
                phases,
                "lifecycle",
                &error,
            ),
            1,
        ));
    }
    let dispatcher = match crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(
        &recipe.promotion_transport["attempt_dispatch"],
    ) {
        Ok(dispatcher) => {
            phases.push(
                serde_json::json!({ "phase": "transport", "status": "passed", "reason": "ok" }),
            );
            dispatcher
        }
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    candidate_fingerprint,
                    phases,
                    "transport",
                    &error,
                ),
                1,
            ))
        }
    };
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .expect("continuation selection is recipe-bound");
    let terminal_review =
        agent_task_service::terminal_review_form_continuation_is_eligible(&attempt.plan, &record)?;
    let historical_terminal =
        agent_task_service_direct::historical_terminal_continuation_is_eligible(
            &recipe,
            record.state,
        );
    let mut options = match if terminal_review || historical_terminal {
        agent_task_service::reconstruct_adoption_options_with_dispatcher(&recipe, dispatcher)
    } else {
        agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)
    } {
        Ok(options) => options,
        Err(error) => {
            return Ok((
                cook_continuation_preflight_report(
                    selected_run_id,
                    candidate_fingerprint,
                    phases,
                    "recipe",
                    &error,
                ),
                1,
            ))
        }
    };
    options.initial_run_id = attempt.run_id.clone();
    options.initial_plan = attempt.plan.clone();
    if let Err(error) = agent_task_service::preflight_cook_continuation_admission(&options) {
        return Ok((
            cook_continuation_preflight_report(
                selected_run_id,
                candidate_fingerprint,
                phases,
                "provider_workspace_baseline",
                &error,
            ),
            1,
        ));
    }
    phases.push(serde_json::json!({ "phase": "provider_workspace_baseline", "status": "passed", "reason": "ok" }));
    phases.push(
        serde_json::json!({ "phase": "candidate_admission", "status": "passed", "reason": "ok" }),
    );
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-continue-preflight/v1",
            "admitted": true,
            "selected_attempt": { "run_id": selected_run_id },
            "candidate_fingerprint": candidate_fingerprint,
            "continuation": {
                "path": if historical_terminal {
                    "historical_terminal"
                } else if terminal_review {
                    "terminal_review_form"
                } else {
                    "current_runtime"
                },
                "provider_replay": terminal_review
            },
            "phases": phases,
            "side_effects": { "provider_dispatch": false, "git_mutation": false, "github_mutation": false, "finalization": false }
        }),
        0,
    ))
}

fn cook_continuation_preflight_report(
    selected_run_id: Option<String>,
    candidate_fingerprint: Value,
    mut phases: Vec<Value>,
    phase: &str,
    error: &homeboy::core::Error,
) -> Value {
    phases.push(serde_json::json!({ "phase": phase, "status": "blocked", "reason": format!("{:?}", error.code), "message": error.message }));
    serde_json::json!({
        "schema": "homeboy/agent-task-cook-continue-preflight/v1",
        "admitted": false,
        "selected_attempt": { "run_id": selected_run_id },
        "candidate_fingerprint": candidate_fingerprint,
        "phases": phases,
        "side_effects": { "provider_dispatch": false, "git_mutation": false, "github_mutation": false, "finalization": false }
    })
}

fn cook_terminal_continuation_status(
    cook_id: &str,
    run_id: &str,
    provider_state: &str,
    state: agent_task_service::CookContinuationState,
) -> Value {
    let (status, guidance) = match state {
        agent_task_service::CookContinuationState::Pending => (
            "continuation_pending",
            serde_json::json!({
                "action": "await_continuation_claim",
                "command": agent_task_service_direct::cook_continue_command(None, run_id, false, None),
            }),
        ),
        agent_task_service::CookContinuationState::Claimed => (
            "continuation_in_progress",
            serde_json::json!({
                "action": "await_claimed_continuation",
                "command": format!("homeboy agent-task status {run_id} --full"),
            }),
        ),
        agent_task_service::CookContinuationState::Failed => (
            "continuation_recovery_required",
            serde_json::json!({
                "action": "rearm_failed_continuation",
                "command": agent_task_service_direct::cook_continue_command(None, run_id, true, None),
            }),
        ),
        agent_task_service::CookContinuationState::Completed => (
            "continuation_completed",
            serde_json::json!({
                "action": "inspect_completed_cook",
                "command": format!("homeboy agent-task status {run_id} --full"),
            }),
        ),
        agent_task_service::CookContinuationState::Absent => (
            "continuation_not_scheduled",
            serde_json::json!({
                "action": "reconcile_terminal_attempt",
                "command": format!("homeboy agent-task reconcile {run_id} --dry-run"),
            }),
        ),
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": status,
        "provider": { "state": provider_state, "run_id": run_id },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "guidance": guidance,
    })
}

fn cook_continuation_status(
    cook_id: &str,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Value {
    let run_id = &record.run_id;
    let runner_id = record.runner_id();
    let waiting_for_capacity = record
        .metadata
        .pointer("/runner_queue/state")
        .and_then(Value::as_str)
        == Some("waiting_for_capacity");
    let runner_disconnected = record
        .metadata
        .get("runner_liveness")
        .and_then(Value::as_str)
        == Some("disconnected");
    let (status, guidance) = if runner_disconnected {
        (
            "recovery_required",
            serde_json::json!({
                "action": "reconcile_runner_authority",
                "command": format!("homeboy agent-task reconcile {run_id} --dry-run"),
                "message": "The runner could not be reached during bounded reconciliation; inspect the scoped recovery before retrying provider work."
            }),
        )
    } else if waiting_for_capacity
        || record.state == agent_task_lifecycle::AgentTaskRunState::Queued
    {
        let command = runner_id
            .map(|runner_id| format!("homeboy runner status {runner_id}"))
            .unwrap_or_else(|| format!("homeboy agent-task run {run_id}"));
        (
            "accepted_unscheduled",
            serde_json::json!({
                "action": if runner_id.is_some() { "await_runner_capacity" } else { "schedule_queued_run" },
                "command": command,
                "message": if runner_id.is_some() {
                    "The runner accepted this Cook and owns its FIFO queue entry; it will schedule provider execution when a capacity lease is available."
                } else {
                    "This Cook is durably queued but has no runner or provider boundary; schedule its queued run before expecting provider work."
                }
            }),
        )
    } else {
        (
            "observation_in_progress",
            serde_json::json!({
                "action": "watch_provider",
                "command": format!("homeboy agent-task logs {run_id}"),
                "message": "The runner reports active provider work; follow the durable observation until its terminal result is projected."
            }),
        )
    };
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": status,
        "provider": {
            "state": format!("{:?}", record.state),
            "run_id": run_id,
            "runner_id": runner_id,
            "runner_job_status": record.metadata.get("runner_job_status"),
        },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "guidance": guidance,
    })
}

fn cook_report_with_continuation(mut value: Value) -> Value {
    if value.get("status").and_then(Value::as_str) != Some("in_flight") {
        return value;
    }
    let run_id = value
        .get("latest_run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_state = value
        .get("attempts")
        .and_then(Value::as_array)
        .and_then(|attempts| attempts.last())
        .and_then(|attempt| attempt.get("run_state"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Value::Object(report) = &mut value {
        report.insert(
            "provider".to_string(),
            serde_json::json!({ "state": provider_state, "run_id": run_id }),
        );
        report.insert(
            "remaining_phases".to_string(),
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"]),
        );
        report.insert(
            "continuation_command".to_string(),
            serde_json::json!(agent_task_service_direct::cook_continue_command(
                None, &run_id, false, None
            )),
        );
    }
    value
}

/// Converge a Cook promotion destination before compiling a task plan. This is
/// controller-owned so local and Lab dispatch use the same managed checkout.
pub(crate) fn provision_cook_destination(args: &AgentTaskCookArgs) -> homeboy::core::Result<Value> {
    let to_worktree = args.to_worktree.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--to-worktree is required before provisioning a Cook destination".to_string(),
        ])
    })?;
    if let Some(cwd) = args.dispatch.cwd.as_deref() {
        let cwd = Path::new(cwd);
        ensure_active_managed_cook_destination(to_worktree)?;
        homeboy::core::worktree_providers::validate_task_worktree_root(cwd, to_worktree)?;
        let path = std::fs::canonicalize(cwd).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(cwd.display().to_string()))
        })?;
        validate_cook_destination_identity(args, &path)?;
        let logical_provider_provenance =
            validate_cook_cwd_destination_identity(&path, to_worktree)?;
        let mut provision = serde_json::json!({
            "action": "existing",
            "kind": "explicit_cwd",
            "handle": to_worktree,
            "path": path,
        });
        if let Some(provenance) = logical_provider_provenance {
            provision["logical_provider_provenance"] = provenance;
        }
        return Ok(provision);
    }
    let direct_path = Path::new(to_worktree);
    if direct_path.is_dir() {
        homeboy::core::worktree_providers::validate_task_worktree_root(direct_path, to_worktree)?;
        let path = std::fs::canonicalize(direct_path).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(direct_path.display().to_string()),
            )
        })?;
        return Ok(serde_json::json!({
            "action": "existing",
            "kind": "direct_task_worktree",
            "handle": to_worktree,
            "path": path,
        }));
    }
    if let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(to_worktree)? {
        if record.state() != &homeboy::core::worktree::TaskWorktreeState::Active {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace `{}` is no longer active",
                    record.handle()
                ),
                Some(to_worktree.to_string()),
                None,
            ));
        }
        let path = PathBuf::from(record.path());
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, to_worktree)?;
        return Ok(
            serde_json::json!({ "action": "existing", "kind": record.source_kind(), "handle": to_worktree, "path": path }),
        );
    }

    let config = defaults::load_config();
    // A command provider's answer is external, bounded, and retryable. Preserve
    // the complete creation intent in the Cook plan and resolve it only after
    // Cook has materialized its durable recipe/run identity.
    if config.worktree_providers.values().any(|provider| {
        provider.enabled
            && provider.apply_enabled
            && (provider.commands.resolve_identity.is_some()
                || provider.commands.resolve.is_some()
                || provider.commands.list.is_some()
                || provider.commands.ensure.is_some())
    }) {
        return Ok(serde_json::json!({
            "action": "lookup_pending",
            "kind": "provider",
            "handle": to_worktree,
            "provision_intent": {
                "repo": args.dispatch.repo,
                "base": args.base,
                "head": args.head,
                "task_url": args.dispatch.task_url,
            },
        }));
    }
    match homeboy::core::worktree_providers::resolve_apply_enabled_worktree_provider_identity_from_config(to_worktree, &config) {
        Ok(identity) => {
            homeboy::core::worktree_providers::validate_task_worktree_root(
                Path::new(&identity.path),
                to_worktree,
            )?;
            validate_cook_destination_identity(args, Path::new(&identity.path))?;
            match homeboy::core::worktree_providers::attest_apply_enabled_worktree_provider_safety_from_config(&identity, &config) {
                Ok(safety) if safety.fresh && !safety.dirty && !safety.unpushed && !identity.primary => return Ok(serde_json::json!({
                    "action": "existing", "kind": "provider", "provider": identity.provider_id, "handle": identity.handle, "path": identity.path, "branch": identity.branch,
                    "workspace_identity": identity, "workspace_safety": safety,
                })),
                Ok(_) => return Err(homeboy::core::Error::validation_invalid_argument("to_worktree", "worktree provider safety attestation is not safe for mutation", Some(to_worktree.to_string()), None)),
                Err(error) if error.details["worktree_provider_split"] == "timed_out" => return Ok(serde_json::json!({
                    "action": "attestation_pending", "kind": "provider", "provider": identity.provider_id, "handle": identity.handle, "path": identity.path, "branch": identity.branch,
                    "worktree_provider_id": identity.provider_id,
                    "workspace_identity": identity,
                    "workspace_safety": { "state": "timed_out", "latency_ms": error.details["latency_ms"], "budget_ms": error.details["budget_ms"] },
                })),
                Err(error) => return Err(error),
            }
        }
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("not_found") => {}
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("timed_out") =>
        {
            // The lookup is bounded, but a timeout says nothing about whether
            // this exact handle exists. Carry only the declared handle into
            // Cook; durable materialization retries the same exact provider.
            let provider_id = error
                .details
                .get("worktree_provider_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    homeboy::core::Error::internal_unexpected(
                        "timed-out worktree provider lookup omitted its provider identity"
                            .to_string(),
                    )
                })?;
            return Ok(serde_json::json!({
                "action": "lookup_pending",
                "kind": "provider",
                "handle": to_worktree,
                "worktree_provider_id": provider_id,
            }));
        }
        Err(error) => return Err(error),
    }

    let intent = cook_workspace_create_intent(args)?;
    provision_apply_enabled_worktree_provider_from_config(&intent, &config).map(|provision| {
        serde_json::json!({
            "action": provision.action,
            "provider": provision.resolution.provider_id,
            "idempotency_key": provision.idempotency_key,
            "handle": provision.resolution.worktree.handle,
            "path": provision.resolution.worktree.path,
            "branch": provision.resolution.worktree.branch,
            "intent": cook_workspace_plan_identity(&intent),
        })
    })
}

/// The exact declaration used both by preview planning and live provider
/// provisioning. Keeping it as one value prevents a previewed branch or handle
/// from drifting from the later ensure request.
fn cook_workspace_create_intent(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<WorktreeProviderCreateIntent> {
    Ok(WorktreeProviderCreateIntent {
        handle: args.to_worktree.clone().ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec![
                "--to-worktree is required to create a missing Cook destination".to_string(),
            ])
        })?,
        repo: args.dispatch.repo.clone().ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec![
                "--repo <repo> is required to create a missing --to-worktree destination"
                    .to_string(),
            ])
        })?,
        base: args.base.clone(),
        head: args.head.clone().ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec![
                "--head <branch> is required to create a missing --to-worktree destination"
                    .to_string(),
            ])
        })?,
        task_url: args.dispatch.task_url.clone().ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec![
                "--task-url <url> is required to create a missing --to-worktree destination"
                    .to_string(),
            ])
        })?,
    })
}

fn cook_workspace_plan_identity(intent: &WorktreeProviderCreateIntent) -> Value {
    serde_json::json!({
        "handle": intent.handle,
        "repo": intent.repo,
        "base": intent.base,
        "head": intent.head,
        "task_url": intent.task_url,
    })
}

fn ensure_active_managed_cook_destination(to_worktree: &str) -> homeboy::core::Result<()> {
    let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(to_worktree)?
    else {
        return Ok(());
    };
    if record.state() != &homeboy::core::worktree::TaskWorktreeState::Active {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "Homeboy workspace `{}` is no longer active",
                record.handle()
            ),
            Some(to_worktree.to_string()),
            None,
        ));
    }
    Ok(())
}

/// An explicit CWD is the Cook workspace authority. A co-supplied destination
/// can only name that same existing worktree; never consult a provider merely
/// to rediscover a path the operator already supplied.
fn validate_cook_cwd_destination_identity(
    cwd: &Path,
    to_worktree: &str,
) -> homeboy::core::Result<Option<Value>> {
    let (destination, logical_provider_provenance) = if Path::new(to_worktree).is_dir() {
        std::fs::canonicalize(to_worktree).map(|path| (path, None))
    } else if let Some(record) =
        homeboy::core::worktree::resolve_workspace_ref_if_present(to_worktree)?
    {
        ensure_active_managed_cook_destination(to_worktree)?;
        std::fs::canonicalize(record.path()).map(|path| (path, None))
    } else {
        // The supplied CWD is authoritative. An external handle must retain
        // the complete canonical checkout basename, which is injective and
        // avoids a provider lookup on this local authority boundary.
        validate_logical_worktree_handle_path_relationship(cwd, to_worktree)?;
        Ok((
            cwd.to_path_buf(),
            Some(serde_json::json!({
                "schema": "homeboy/logical-worktree-handle-provenance/v1",
                "handle": to_worktree,
                "canonical_path": cwd,
                "validation": "exact_handle_basename",
            })),
        ))
    }
    .map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(to_worktree.to_string()))
    })?;
    if destination != cwd {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "to_worktree",
            "--cwd and --to-worktree must resolve to the same linked task worktree",
            Some(to_worktree.to_string()),
            None,
        ));
    }
    Ok(logical_provider_provenance)
}

pub(crate) fn validate_logical_worktree_handle_path_relationship(
    cwd: &Path,
    handle: &str,
) -> homeboy::core::Result<()> {
    let basename = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cwd",
                "--cwd must have a UTF-8 worktree basename",
                Some(cwd.display().to_string()),
                None,
            )
        })?;
    if handle == basename {
        return Ok(());
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "to_worktree",
        "--cwd and --to-worktree must name the same linked task worktree",
        Some(handle.to_string()),
        None,
    ))
}

pub(crate) fn resolve_cook_destination(
    mut args: AgentTaskCookArgs,
) -> homeboy::core::Result<AgentTaskCookArgs> {
    normalize_cook_repository_identity(&mut args)?;
    if args.to_worktree.is_some() {
        return Ok(args);
    }
    if let Some(cwd) = args.dispatch.cwd.as_deref() {
        let cwd = std::fs::canonicalize(cwd).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(cwd.to_string()))
        })?;
        // A supplied checkout is the writable Cook authority. Preserve its
        // canonical path instead of deriving an unrelated issue handle.
        args.to_worktree = Some(cwd.display().to_string());
        return Ok(args);
    }
    let repo = args.dispatch.repo.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--repo <repo> is required when --to-worktree is omitted".to_string(),
        ])
    })?;
    let task_url = args.dispatch.task_url.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--task-url <url> is required when --to-worktree is omitted".to_string(),
        ])
    })?;
    let config = defaults::load_config();
    // DMC's provider mapping currently exposes safety and handle metadata but
    // not task ownership. In that mode the canonical handle is still resolved
    // first; its ensure operation must reject a different task-owned worktree.
    args.to_worktree = Some(match homeboy::core::worktree_providers::find_apply_enabled_worktree_provider_by_task_url_from_config(task_url, &config) {
        Ok(Some(resolution)) => resolution.worktree.handle,
        Ok(None) => format!("{repo}@{}", slugify_cook_branch(&derived_cook_branch(task_url)?)),
        Err(mut error) => {
            if let Some(handles) = error.message.strip_prefix(&format!("multiple active apply-enabled worktrees are owned by `{task_url}`: ")) {
                error.details["recovery"] = serde_json::json!(handles.split(", ").map(|handle| format!("homeboy agent-task cook --to-worktree {handle}")).collect::<Vec<_>>());
            }
            return Err(error);
        }
    });
    if args.head.is_none() {
        args.head = Some(derived_cook_branch(task_url)?);
    }
    Ok(args)
}

/// Preview performs bounded provider resolution and may run a provider's
/// declared read-only plan command. It never invokes provider mutation or task
/// execution, and reports remote destinations as a typed materialization
/// requirement before filesystem-dependent planning.
fn resolve_cook_preview_destination(
    args: AgentTaskCookArgs,
) -> homeboy::core::Result<(AgentTaskCookArgs, Value)> {
    let issue_derived = args.to_worktree.is_none() && args.dispatch.cwd.is_none();
    let args = resolve_cook_destination(args)?;
    let handle = args.to_worktree.clone().expect("preview destination set");
    let path = if let Some(cwd) = args.dispatch.cwd.as_deref() {
        let path = std::fs::canonicalize(cwd).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(cwd.to_string()))
        })?;
        ensure_active_managed_cook_destination(&handle)?;
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, &handle)?;
        validate_cook_destination_identity(&args, &path)?;
        validate_cook_cwd_destination_identity(&path, &handle)?;
        path
    } else if Path::new(&handle).is_dir() {
        let path = std::fs::canonicalize(&handle).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(handle.clone()))
        })?;
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, &handle)?;
        validate_cook_destination_identity(&args, &path)?;
        path
    } else if let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(&handle)?
    {
        ensure_active_managed_cook_destination(&handle)?;
        let path = PathBuf::from(record.path());
        if !path.is_dir() {
            return Err(preview_destination_blocker(
                &handle,
                "the registered workspace path is missing",
            ));
        }
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, &handle)?;
        validate_cook_destination_identity(&args, &path)?;
        path
    } else {
        let config = defaults::load_config();
        let identity = match homeboy::core::worktree_providers::resolve_apply_enabled_worktree_provider_identity_from_config(&handle, &config) {
            Ok(identity) => identity,
            Err(error)
                if issue_derived && error.details["worktree_provider_lookup"] == "not_found" =>
            {
                let intent = cook_workspace_create_intent(&args)?;
                let plan = match plan_apply_enabled_worktree_provider_from_config(&intent, &config) {
                    Ok(plan) => plan,
                    Err(error) => {
                        return Ok((
                            args,
                            serde_json::json!({
                                "action": "unresolved_provider",
                                "kind": "provider",
                                "handle": handle,
                                "provider_id": error.details["worktree_provider_id"],
                                "reason": error.message,
                                "details": error.details,
                            }),
                        ));
                    }
                };
                let WorktreeProviderCreatePlan::WouldCreate(resolution) = plan
                else {
                    return Err(homeboy::core::Error::internal_unexpected(
                        "worktree provider changed from absent to existing while previewing Cook".to_string(),
                    ));
                };
                return Ok((
                    args,
                    serde_json::json!({
                        "action": "planned_create",
                        "kind": "provider",
                        "handle": resolution.worktree.handle,
                        "path": resolution.worktree.path,
                        "branch": resolution.worktree.branch,
                        "provider_id": resolution.provider_id,
                        "intent": cook_workspace_plan_identity(&intent),
                    }),
                ));
            }
            Err(error) if issue_derived => return Ok((
                args,
                serde_json::json!({
                    "action": "unresolved_provider",
                    "kind": "provider",
                    "handle": handle,
                    "provider_id": error.details["worktree_provider_id"],
                    "reason": error.message,
                    "details": error.details,
                }),
            )),
            Err(error) => return Err(error),
        };
        if homeboy::core::worktree_providers::worktree_provider_path_requires_materialization(
            &identity.path,
        ) {
            return Ok((
                args,
                serde_json::json!({
                    "action": "materialization_required",
                    "kind": "provider",
                    "handle": handle,
                    "provider_id": identity.provider_id,
                    "remote_path": identity.path,
                    "reason": "the configured provider resolved a registered remote workspace that requires materialization before filesystem planning",
                    "apply": "rerun Cook without --preview to converge the destination through its configured provider",
                }),
            ));
        }
        let path = PathBuf::from(&identity.path);
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, &handle)?;
        validate_cook_destination_identity(&args, &path)?;
        return Ok((
            args,
            serde_json::json!({
                "action": "planned_reuse",
                "kind": "provider",
                "handle": identity.handle,
                "path": identity.path,
                "branch": identity.branch,
                "provider_id": identity.provider_id,
            }),
        ));
    };
    Ok((
        args,
        serde_json::json!({
            "action": "planned_reuse",
            "kind": "preview_local",
            "handle": handle,
            "path": path,
        }),
    ))
}

fn preview_destination_blocker(handle: &str, problem: &str) -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "to_worktree",
        format!("preview unresolved destination `{handle}`: {problem}"),
        Some(handle.to_string()),
        Some(vec!["Pass an existing local --cwd or --to-worktree path, or run Cook to let its configured provider resolve the destination.".to_string()]),
    )
}

#[derive(Debug, Clone)]
struct CookRepositoryIdentity {
    slug: String,
    remote_identity: String,
    workspace_path: PathBuf,
    provenance: String,
}

fn normalize_cook_repository_identity(args: &mut AgentTaskCookArgs) -> homeboy::core::Result<()> {
    let mut identities = Vec::new();
    let mut source_identities = Vec::new();
    for (flag, value) in [
        ("--workspace", args.dispatch.workspace.as_deref()),
        ("--cwd", args.dispatch.cwd.as_deref()),
    ] {
        let Some(value) = value else {
            continue;
        };
        let resolved = cook_repository_identities_for_workspace(flag, value)?;
        source_identities.push((flag, resolved.clone()));
        identities.extend(resolved);
    }
    if identities.is_empty() {
        if args.dispatch.workspace.is_some() || args.dispatch.cwd.is_some() {
            return require_explicit_cook_repo(
                args,
                "the supplied workspace is not a Git checkout with a configured repository remote",
            );
        }
        return bind_cook_repository_identity_from_config(args);
    }

    let source_remotes = source_identities
        .iter()
        .flat_map(|(_, identities)| {
            identities
                .iter()
                .map(|identity| identity.remote_identity.clone())
        })
        .collect::<std::collections::BTreeSet<_>>();
    if source_remotes.len() != 1
        || source_identities
            .iter()
            .any(|(_, identities)| identities.is_empty())
    {
        return Err(repository_identity_conflict_error(&source_identities));
    }

    let candidates: BTreeMap<_, _> =
        identities
            .into_iter()
            .fold(BTreeMap::new(), |mut candidates, identity| {
                candidates.entry(identity.slug.clone()).or_insert(identity);
                candidates
            });
    let selected = match args.dispatch.repo.as_deref() {
        Some(repo) => candidates.get(repo).cloned().ok_or_else(|| {
            repository_identity_error(
                format!("--repo `{repo}` does not match the supplied workspace repository"),
                &candidates,
            )
        })?,
        None if candidates.len() == 1 => {
            candidates.values().next().cloned().expect("one candidate")
        }
        None => {
            return Err(repository_identity_error(
                "the supplied workspace maps to multiple configured repositories".to_string(),
                &candidates,
            ));
        }
    };
    args.dispatch.repo = Some(selected.slug.clone());
    args.repository_identity = Some(serde_json::json!({
        "slug": selected.slug,
        "remote_identity": selected.remote_identity,
        "workspace_path": selected.workspace_path,
        "provenance": selected.provenance,
    }));
    Ok(())
}

/// A repo-only Cook has no local checkout to attest before a deferred provider
/// lookup. Prefer configured remote identity; otherwise retain a normalized
/// repository name that the resolved checkout must prove through its remote.
fn bind_cook_repository_identity_from_config(
    args: &mut AgentTaskCookArgs,
) -> homeboy::core::Result<()> {
    let Some(repo) = args.dispatch.repo.as_deref() else {
        return Ok(());
    };
    let component = homeboy::core::component::registered_by_id(repo)?;
    args.repository_identity = Some(
        match component
            .as_ref()
            .and_then(|component| component.remote_url.as_deref())
            .and_then(canonical_remote_identity)
        {
            Some(remote_identity) => serde_json::json!({
                "slug": repo,
                "remote_identity": remote_identity,
                "provenance": "--repo:configured-component",
            }),
            None => serde_json::json!({
                "slug": repo,
                "repository_name": normalize_repository_name(repo),
                "provenance": "--repo:requested-repository",
            }),
        },
    );
    Ok(())
}

fn normalize_repository_name(repository: &str) -> String {
    repository
        .trim()
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn require_explicit_cook_repo(args: &AgentTaskCookArgs, reason: &str) -> homeboy::core::Result<()> {
    if args.dispatch.repo.is_some() {
        return Ok(());
    }
    Err(homeboy::core::Error::validation_missing_argument(vec![
        format!(
            "--repo <repo> is required because {reason}; provide --repo <configured-component>"
        ),
    ]))
}

fn cook_repository_identities_for_workspace(
    flag: &str,
    value: &str,
) -> homeboy::core::Result<Vec<CookRepositoryIdentity>> {
    let path = Path::new(value);
    let workspace_path = if path.is_dir() {
        std::fs::canonicalize(path).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?
    } else if let Some(record) = homeboy::core::worktree::resolve_workspace_ref_if_present(value)? {
        PathBuf::from(record.path())
    } else {
        return Ok(Vec::new());
    };
    let Some(git_root) = homeboy::core::git::repo_root(&workspace_path) else {
        return Ok(Vec::new());
    };
    let remotes = homeboy::core::git::output_optional(&git_root, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .filter_map(|remote| {
            homeboy::core::git::remote_url(&git_root, remote).map(|url| (remote.to_string(), url))
        })
        .collect::<Vec<_>>();
    let configured = homeboy::core::component::registered()?;
    let mut identities = Vec::new();
    for (remote_name, remote_url) in remotes {
        let Some(remote_identity) = canonical_remote_identity(&remote_url) else {
            continue;
        };
        for component in &configured {
            let Some(component_identity) = component
                .remote_url
                .as_deref()
                .and_then(canonical_remote_identity)
            else {
                continue;
            };
            if component_identity == remote_identity {
                identities.push(CookRepositoryIdentity {
                    slug: component.id.clone(),
                    remote_identity: remote_identity.clone(),
                    workspace_path: git_root.clone(),
                    provenance: format!("{flag}:git-remote:{remote_name}"),
                });
            }
        }
    }
    Ok(identities)
}

pub(crate) fn canonical_remote_identity(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim();
    let (host, path) = if let Some((_, rest)) = remote_url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = remote_url.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    (!host.is_empty()
        && path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
            >= 2)
        .then(|| {
            format!(
                "git://{}/{}",
                host.to_ascii_lowercase(),
                path.to_ascii_lowercase()
            )
        })
}

fn repository_identity_conflict_error(
    sources: &[(&str, Vec<CookRepositoryIdentity>)],
) -> homeboy::core::Error {
    let candidates = sources
        .iter()
        .map(|(flag, identities)| {
            let identities = identities
                .iter()
                .map(|identity| identity.remote_identity.as_str())
                .collect::<Vec<_>>();
            format!(
                "{flag}: {}",
                if identities.is_empty() {
                    "unresolved".to_string()
                } else {
                    identities.join(", ")
                }
            )
        })
        .collect::<Vec<_>>();
    homeboy::core::Error::validation_invalid_argument(
        "workspace",
        format!(
            "--workspace and --cwd must resolve to the same configured repository identity; --repo cannot select one conflicting checkout: {}",
            candidates.join("; ")
        ),
        None,
        None,
    )
}

fn validate_cook_destination_identity(
    args: &AgentTaskCookArgs,
    destination: &Path,
) -> homeboy::core::Result<()> {
    let identity = args.repository_identity.as_ref();
    homeboy::core::worktree_providers::validate_task_worktree_repository_identity(
        destination,
        identity
            .and_then(|identity| identity.get("remote_identity"))
            .and_then(Value::as_str),
        identity
            .and_then(|identity| identity.get("repository_name"))
            .and_then(Value::as_str),
    )
}

fn repository_identity_error(
    message: String,
    candidates: &BTreeMap<String, CookRepositoryIdentity>,
) -> homeboy::core::Error {
    let candidates = candidates
        .values()
        .map(|candidate| {
            format!(
                "{} ({}, {})",
                candidate.slug,
                candidate.remote_identity,
                candidate.workspace_path.display()
            )
        })
        .collect::<Vec<_>>();
    let recovery = candidates
        .iter()
        .map(|candidate| {
            let slug = candidate
                .split_whitespace()
                .next()
                .expect("candidate has slug");
            format!("homeboy agent-task cook --repo {slug} ...")
        })
        .collect::<Vec<_>>();
    homeboy::core::Error::validation_invalid_argument(
        "repo",
        format!("{message}; candidates: {}", candidates.join(", ")),
        None,
        Some(recovery),
    )
}

fn derived_cook_branch(task_url: &str) -> homeboy::core::Result<String> {
    let issue = task_url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let Some((repository, number)) = issue.rsplit_once("/issues/") else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must be a GitHub issue URL when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    };
    let number = number.split('/').next().unwrap_or_default();
    if number.is_empty() || !number.chars().all(|character| character.is_ascii_digit()) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must end in a numeric GitHub issue number when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    }
    let mut segments = repository.trim_end_matches('/').rsplit('/');
    let repo = segments.next().unwrap_or_default();
    let owner = segments.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must include a GitHub owner and repo when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    }
    Ok(format!("fix/issue-{number}-{}", slugify_cook_branch(repo)))
}

fn slugify_cook_branch(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn record_cook_provision(plan: &mut AgentTaskPlan, provision: Value) {
    if let Some(task) = plan.tasks.first_mut() {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["worktree_provision"] = provision;
    }
}

pub(crate) fn validate_cook_request(args: &AgentTaskCookArgs) -> homeboy::core::Result<()> {
    validate_cook_request_with_provenance(args, None)
}

/// Reject a Cook whose selected backend cannot execute, before the destination
/// worktree is provisioned.
///
/// `agent-task providers` reporting `available` used to mean only "declared",
/// so a Cook could be dispatched to a backend with no credential, materialize a
/// workspace, and burn its whole execution budget discovering the gap inside
/// the provider — while other configured backends were sitting there usable
/// (#11479). The backend's declared credentials are knowable here, so the
/// remediation is reported instead of a spent budget.
///
/// A backend that cannot be resolved at all is left to the resolution
/// validators; this only speaks about credentials.
pub(crate) fn preflight_cook_provider_credentials(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<()> {
    let catalog = provider::AgentTaskProviderCatalog::discover();
    preflight_cook_provider_credentials_with_catalog(dispatch_args_for_cook(args).into(), &catalog)
}

pub(crate) fn preflight_cook_provider_credentials_with_catalog(
    dispatch: dispatch_service::AgentTaskDispatchCommand,
    catalog: &provider::AgentTaskProviderCatalog,
) -> homeboy::core::Result<()> {
    let route =
        dispatch_service::resolve_cook_initial_provider_route_with_catalog(dispatch, &catalog)?;
    provider::preflight_provider_credentials_for_backend(
        catalog.providers(),
        &route.backend,
        route.selector.as_deref(),
    )
}

/// Validates Cook input authority before destination provisioning, provider
/// discovery, or any other external effect starts.
pub(crate) fn validate_cook_request_with_provenance(
    args: &AgentTaskCookArgs,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> homeboy::core::Result<()> {
    if args.no_finalize {
        if let Some(provenance) = provenance {
            provenance
                .require_sources(
                    &["no_finalize"],
                    &[crate::cli_surface::ArgumentSource::CommandLine],
                )
                .map_err(|error| {
                    homeboy::core::Error::validation_invalid_argument(
                        "no_finalize",
                        "--no-finalize must be explicitly authorized on the command line",
                        Some(serde_json::to_string(&error).expect("source policy serializes")),
                        None,
                    )
                })?;
        }
    }
    if args.goal.is_some() && !args.dispatch.tasks.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task",
            "agent-task cook uses --goal as framing metadata and --prompt as its one source; --goal and --task conflict",
            None,
            Some(vec![
                "Use: homeboy agent-task cook --goal 'Describe the outcome' --prompt @task.txt --to-worktree sample-plugin@fix-issue --verify 'npm test'".to_string(),
            ]),
        ));
    }
    validate_provider_evidence_inputs(
        &args.provider_evidence_inputs,
        args.dispatch.prompt.as_deref(),
    )?;
    let dispatch = dispatch_args_for_cook(args);
    dispatch_service::validate_single_cook_prompt_source(
        dispatch.prompt.as_deref(),
        &dispatch.tasks,
        dispatch.core.tasks_json.as_deref(),
    )?;
    if !args.gates.has_deterministic_gate() && !args.no_finalize {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task cook requires at least one deterministic --verify or --private-verify gate before it can commit, push, and open a PR",
            None,
            Some(vec![
                "Provide a deterministic verification gate, e.g. --verify \"cargo test\"."
                    .to_string(),
                "Use --no-finalize for a read-only Cook that will not commit, push, or open a PR."
                    .to_string(),
            ]),
        ));
    }
    if args.dispatch.core.queue_only {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "queue-only",
            "agent-task cook cannot queue its controller-owned lifecycle",
            None,
            None,
        ));
    }
    // Resolve against the same filtered rotation policy that compilation uses,
    // while Cook is still in its no-side-effect validation phase.
    let request = dispatch_service::resolve_dispatch_request(dispatch.into())?;
    let configured_rotations = dispatch_service::controller_resolved_execution_policy(&request)
        .rotation
        .as_ref()
        .map(|rotation| {
            rotation
                .max_total_attempts()
                .min(
                    u32::try_from(rotation.entries.len())
                        .unwrap_or(u32::MAX)
                        .saturating_add(1),
                )
                .saturating_sub(1)
        })
        .unwrap_or(0);
    homeboy::agents::agent_task_service::resolve_cook_budget(
        args.max_attempts,
        configured_rotations,
        args.dispatch.core.attempts,
        args.dispatch.core.same_provider_retries,
        args.dispatch.core.provider_rotations,
    )?;
    Ok(())
}

pub(crate) fn promotion_provider(args: PromotionProviderArgs) -> CmdResult<Value> {
    let mut request = Vec::new();
    std::io::stdin()
        .take(MAX_PROMOTION_PROVIDER_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("read agent-task promotion provider request".to_string()),
            )
        })?;
    if request.len() as u64 > MAX_PROMOTION_PROVIDER_REQUEST_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!(
                "promotion provider request exceeds {MAX_PROMOTION_PROVIDER_REQUEST_BYTES} bytes"
            ),
            None,
            None,
        ));
    }
    let request = String::from_utf8(request).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!("promotion provider request is not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    homeboy::agents::agent_task_promotion::apply_materialized_workspace_patch(
        Path::new(&args.workspace),
        &request,
    )
    .and_then(|response| {
        serde_json::from_str(&response).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize agent-task promotion provider response".to_string()),
            )
        })
    })
    .map(|value| (value, 0))
}

pub(super) fn run_cook_with_executor(
    args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    run_cook_with_executor_and_dispatcher(args, executor, None)
}

pub(crate) fn run_cook_with_executor_and_dispatcher(
    args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
) -> CmdResult<Value> {
    run_cook_with_executor_and_dispatcher_with_progress(
        args,
        executor,
        attempt_dispatcher,
        None,
        None,
    )
}

pub(crate) fn run_cook_with_executor_and_dispatcher_with_progress(
    mut args: AgentTaskCookArgs,
    executor: SharedAgentTaskExecutor,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    progress: super::CookProgress<'_>,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    args.gates.snapshot_file_inputs()?;
    let args = resolve_cook_destination(args)?;
    validate_cook_request_with_provenance(&args, provenance)?;
    let gate_workspace = args.dispatch.cwd.as_deref().map(Path::new).or_else(|| {
        args.to_worktree
            .as_deref()
            .map(Path::new)
            .filter(|path| path.is_dir())
    });
    let gate_contract_validation = validate_gate_contracts(
        args.gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .cloned(),
        gate_workspace,
        &crate::cli_runtime::current_augmented_command_contract(),
    )?;
    // Before any external effect: a backend that cannot execute must say so now
    // rather than after a workspace exists and an execution has been spent
    // (#11479).
    preflight_cook_provider_credentials(&args)?;
    let no_progress = args.no_progress;
    // Deterministic gates exist to make *publication* safe: a green gate is the
    // proof a cook may commit, push, and open a PR. A `--no-finalize` cook does
    // none of those, so a gate is not meaningful there — read-only/exploratory
    // cooks legitimately have nothing to verify. The promotion lifecycle already
    // treats an empty gate set as a vacuously-green run
    // (`run_promotion_gates` -> `PromotionGateRun::without_gates`), so relaxing
    // the requirement here is safe end to end (#7608). Finalizing cooks still
    // require a gate, but now say so with a copy-pasteable example instead of a
    // bare rejection.
    let provision = provision_cook_destination(&args)?;

    let workspace = provision.get("path").and_then(Value::as_str).map(Path::new);
    if let Some(workspace) = workspace {
        project_provider_evidence_inputs(&args.provider_evidence_inputs, workspace, None)?;
    }

    let mut dispatch_args = dispatch_args_for_cook(&args);
    // Resolve @file / stdin / stored-ref prompts before anything consumes the
    // prompt, so the executor receives the exact bytes (#10100).
    resolve_dispatch_prompt(&mut dispatch_args)?;
    rewrite_provider_evidence_prompt(
        &mut dispatch_args.prompt,
        &args.provider_evidence_inputs,
        provision.get("path").and_then(Value::as_str),
    );
    let requested_cook_id = dispatch_args.run_id.clone();
    if let Some(cook_id) = requested_cook_id.as_deref() {
        dispatch_args.run_id = Some(
            args.attempt_run_id
                .clone()
                .unwrap_or_else(|| agent_task_lifecycle::cook_attempt_run_id(cook_id, 1)),
        );
    }
    let run_id = dispatch_args
        .run_id
        .clone()
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    dispatch_args.run_id = Some(run_id.clone());
    let cook_id = requested_cook_id.clone().unwrap_or_else(|| run_id.clone());
    if !no_progress {
        if let Some(progress) = progress {
            progress("preparing", None, None, None)?;
        }
    }
    let (run_id, mut initial_plan) = if let Some(attempt_plan) = args.attempt_plan.as_deref() {
        let run_id = dispatch_args.run_id.clone().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task cook attempt plan requires an attempt run id".to_string(),
            )
        })?;
        (run_id, agent_task_service::read_plan(attempt_plan)?)
    } else {
        let plan = compile_cook_plan(&args, provision.clone())?;
        (run_id, plan)
    };
    if args.attempt_plan.is_some() {
        record_cook_provision(&mut initial_plan, provision);
        record_cook_goal(&mut initial_plan, args.goal.as_deref());
    }
    initial_plan.metadata["gate_contract_validation"] =
        serde_json::to_value(gate_contract_validation)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
    resolve_cook_execution_budget(&args, &mut initial_plan)?;
    if !no_progress {
        eprintln!(
            "{}",
            cook_resolved_policy_disclosure(args.max_attempts, &initial_plan)
        );
        eprintln!("{}", cook_rotation_disclosure(&initial_plan));
        eprintln!("{}", cook_provider_timeout_disclosure(&initial_plan));
    }
    if let Some(provenance) = provenance {
        record_cook_argument_provenance(&mut initial_plan, provenance);
    }
    if args.require_acceptance {
        let authority = args.acceptance_authority.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "acceptance-authority",
                "--require-acceptance requires --acceptance-authority",
                None,
                None,
            )
        })?;
        let policy = args.acceptance_policy.clone().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "acceptance-policy",
                "--require-acceptance requires --acceptance-policy",
                None,
                None,
            )
        })?;
        if authority.trim().is_empty() || policy.trim().is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "acceptance",
                "--require-acceptance requires non-empty --acceptance-authority and --acceptance-policy",
                None,
                None,
            ));
        }
        initial_plan.metadata["acceptance"] =
            serde_json::json!({ "authority": authority, "policy": policy });
    }
    // Capture the resolved task workspace before dispatch. The provider may
    // commit and leave a clean tree, so resolving this after it runs would
    // silently widen the promotion range.
    let source_worktree_path = initial_plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(std::path::PathBuf::from);
    let task_base_sha = source_worktree_path.as_deref().and_then(git_head_sha);
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| default_loop_title(&args));
    let commit_message = args
        .commit_message
        .clone()
        .unwrap_or_else(|| default_loop_commit_message(&args));
    let selected_model = initial_plan
        .tasks
        .first()
        .and_then(|task| task.executor.model())
        .map(str::to_string);
    let durable_observer = |event: &agent_task_service::CookProgressEvent<'_>| {
        if no_progress && event.phase != "durable_identity" {
            return Ok(());
        }
        // The observer renders the activity sample here rather than passing the
        // struct on, so foreground clients (TTY, machine log, `--output` file)
        // all describe a running provider with the same bounded sentence.
        let activity = event.activity_summary();
        progress
            .map(|progress| {
                progress(
                    event.phase,
                    Some(event.cook_id),
                    Some(event.run_id),
                    activity.as_deref(),
                )
            })
            .unwrap_or(Ok(()))
    };
    let result = agent_task_service::run_cook(agent_task_service::CookContext {
        durable_observer: Some(&durable_observer),
        ..agent_task_service::CookContext::new(
            agent_task_service::AgentTaskCookServiceOptions {
            cook_id,
            initial_run_id: run_id,
            initial_plan,
            to_worktree: args.to_worktree.expect("Cook destination is resolved"),
            source_worktree_path,
            provider_command: args.provider_command,
            provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
                argv: args.provider_argv,
                ..Default::default()
            }),
            gates: args.gates.into(),
            max_attempts: args.max_attempts,
            no_finalize: args.no_finalize,
            draft_pr: args.draft_pr,
            base: args.base,
            task_base_sha,
            head: args.head,
            title,
            commit_message,
            source_refs: args.dispatch.task_url.into_iter().collect(),
            protected_branches: args.protected_branches,
            ai_tool: super::fanout::resolve_ai_tool_disclosure(
                &args.ai_tool,
                args.dispatch.backend.as_deref(),
                args.dispatch.selector.as_deref(),
                args.dispatch.model.as_deref(),
            ),
            // Model identity comes only from explicit/config/rotation selection
            // (`--model`, provider profile). Disclosure text like
            // `OpenCode (GPT-5.5)` is presentation, not execution provenance, and
            // must not be reverse-parsed into a model — an omitted model stays
            // omitted so finalization records only real model identity (#9789).
            ai_model: selected_model,
            ai_used_for: args.ai_used_for,
            attempt_dispatcher,
            harvest_context:
                homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process(
                )?,
            },
            executor,
        )
    })?;
    Ok((
        super::status::compact_cook_report(
            cook_report_with_continuation(
                serde_json::to_value(result.value).unwrap_or(Value::Null),
            ),
            args.full,
        ),
        result.exit_code,
    ))
}

pub(crate) fn record_cook_argument_provenance(
    plan: &mut AgentTaskPlan,
    provenance: &crate::cli_surface::CommandArgumentProvenance,
) {
    provenance.project_into(&mut plan.metadata);
}

pub(super) fn dispatch_args_for_cook(args: &AgentTaskCookArgs) -> DispatchArgs {
    let mut dispatch_args = args.dispatch.clone();
    let has_explicit_work = dispatch_args.prompt.is_some()
        || !dispatch_args.tasks.is_empty()
        || dispatch_args.core.tasks_json.is_some();
    if !has_explicit_work {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args
}

fn resolve_cook_execution_budget(
    args: &AgentTaskCookArgs,
    plan: &mut AgentTaskPlan,
) -> homeboy::core::Result<()> {
    let core = &args.dispatch.core;
    let resolved = homeboy::agents::agent_task_service::resolve_cook_budget(
        args.max_attempts,
        plan.options.execution_budget.max_provider_rotations,
        core.attempts,
        core.same_provider_retries,
        core.provider_rotations,
    )?;
    plan.options.execution_budget =
        homeboy::agents::agent_task_scheduler::AgentTaskExecutionBudget::new(
            resolved.provider_executions,
            resolved.same_provider_remediations,
            resolved.provider_rotations,
        );
    plan.metadata["cook_retry_policy"] = serde_json::json!({
        "operator_intent": {
            "max_attempts": args.max_attempts,
            "max_provider_executions": core.attempts,
            "max_same_provider_retries": core.same_provider_retries,
            "max_provider_rotations": core.provider_rotations,
        },
        "resolved": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.provider_rotations,
        },
        "requested": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.requested_provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.requested_provider_rotations,
        },
        "effective": {
            "max_attempts": resolved.requested_attempts,
            "max_provider_executions": resolved.provider_executions,
            "max_same_provider_retries": resolved.same_provider_remediations,
            "max_provider_rotations": resolved.provider_rotations,
        },
        "truncated": {
            "max_provider_rotations": resolved.truncated_provider_rotations,
        },
    });
    Ok(())
}

/// Resolve `--prompt` through the structured-input contract its help already
/// advertises: `@file`, `-` for stdin, and stored `@prompt:<id>` references.
///
/// Cook previously took `--prompt` as a literal string, so a large Markdown
/// prompt had to travel as one shell argument. Backticks, `$` expressions and
/// quotes were then interpreted by the caller's shell before Homeboy ever saw
/// them — in one case executing prompt text as commands (#10100).
///
/// Bytes and newlines are preserved exactly; no trimming.
pub(super) fn resolve_dispatch_prompt(
    dispatch_args: &mut DispatchArgs,
) -> homeboy::core::Result<()> {
    let Some(spec) = dispatch_args.prompt.as_deref() else {
        return Ok(());
    };
    let resolved = homeboy::agents::agent_task_prompts::read_prompt_input(spec)?;
    dispatch_args.prompt = Some(resolved);
    Ok(())
}

#[cfg(test)]
mod rotation_disclosure_tests {
    use super::*;
    use homeboy::agents::agent_task_scheduler::{
        AgentTaskExecutionBudget, AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
    };

    fn plan_with(executions: u32, rotations: u32, entries: usize) -> AgentTaskPlan {
        let mut plan = AgentTaskPlan::new("cook-rotation-disclosure".to_string(), Vec::new());
        plan.options.execution_budget = AgentTaskExecutionBudget::new(executions, 0, rotations);
        plan.options.rotation = (entries > 0).then(|| AgentTaskProviderRotationPolicy {
            entries: (0..entries)
                .map(|index| AgentTaskProviderRotationEntry {
                    model: Some(format!("fallback-model-{index}")),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
        plan
    }

    #[test]
    fn a_funded_rotation_states_the_providers_and_executions_it_will_use() {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(3, 2, 2)),
            "cook: rotation: 2 fallback provider(s), up to 3 provider execution(s)"
        );
    }

    #[test]
    fn no_rotation_at_all_states_that_plainly() {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(1, 0, 0)),
            "cook: rotation: disabled (1 provider execution(s))"
        );
    }

    /// The silence that let #11082 survive: a policy was configured, carried,
    /// and unreachable, and nothing said so.
    #[test]
    fn a_configured_but_unfunded_rotation_is_named_as_unreachable() {
        let disclosure = cook_rotation_disclosure(&plan_with(1, 0, 3));

        assert!(disclosure.contains("disabled"), "{disclosure}");
        assert!(
            disclosure.contains("3 configured rotation provider(s) are unreachable"),
            "{disclosure}"
        );
    }

    #[test]
    fn a_clamped_rotation_discloses_requested_effective_and_truncated_budgets() {
        let mut plan = plan_with(1, 0, 2);
        plan.metadata["cook_retry_policy"] = serde_json::json!({
            "requested": { "max_provider_rotations": 2 },
            "truncated": { "max_provider_rotations": 2 },
        });

        let disclosure = cook_rotation_disclosure(&plan);
        assert!(
            disclosure.contains("requested 2 rotation(s), effective 0, truncated 2"),
            "{disclosure}"
        );
    }

    /// A rotation budget that outruns the execution budget can never fire.
    #[test]
    fn executions_bound_the_rotations_the_disclosure_promises() {
        assert_eq!(
            cook_rotation_disclosure(&plan_with(2, 5, 5)),
            "cook: rotation: 1 fallback provider(s), up to 2 provider execution(s)"
        );
    }
}

#[cfg(test)]
mod prompt_input_tests {
    use super::*;

    fn dispatch_with_prompt(prompt: Option<&str>) -> DispatchArgs {
        DispatchArgs {
            prompt: prompt.map(str::to_string),
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            repo: None,
            task_url: None,
            backend: None,
            selector: None,
            model: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            concurrency: 1,
            run_id: None,
            core: crate::commands::agent_task_dispatch::DispatchCoreArgs {
                tasks_json: None,
                provider_config: None,
                client_context: None,
                attempts: Some(1),
                same_provider_retries: Some(0),
                provider_rotations: Some(0),
                queue_only: false,
                timeout_ms: None,
                resolved_provider_policy: None,
                deny_command: Vec::new(),
                allow_command: Vec::new(),
                command_policy_reason: None,
            },
        }
    }

    /// Shell-sensitive prompt content must reach the executor byte-for-byte,
    /// with no interpolation and no trimming (#10100).
    #[test]
    fn at_file_prompt_is_read_verbatim() {
        let file = tempfile::NamedTempFile::new().expect("prompt file");
        let body = "Run `cargo test` for $HOME\n\nUse \"quotes\" and 'apostrophes'.\n";
        std::fs::write(file.path(), body).expect("write prompt");

        let mut args = dispatch_with_prompt(Some(&format!("@{}", file.path().display())));
        resolve_dispatch_prompt(&mut args).expect("resolve @file prompt");

        assert_eq!(args.prompt.as_deref(), Some(body));
    }

    #[test]
    fn inline_prompt_is_unchanged() {
        let mut args = dispatch_with_prompt(Some("fix the flaky test"));
        resolve_dispatch_prompt(&mut args).expect("resolve inline prompt");

        assert_eq!(args.prompt.as_deref(), Some("fix the flaky test"));
    }

    #[test]
    fn missing_at_file_is_reported_not_passed_through() {
        let mut args = dispatch_with_prompt(Some("@/definitely/not/a/prompt/file/10100"));
        resolve_dispatch_prompt(&mut args).expect_err("a missing prompt file must fail");
    }

    #[test]
    fn empty_at_prefix_is_rejected() {
        let mut args = dispatch_with_prompt(Some("@"));
        let error = resolve_dispatch_prompt(&mut args).expect_err("bare @ is not a path");
        assert!(error.message.contains("missing file path"), "{error:?}");
    }

    #[test]
    fn absent_prompt_is_a_no_op() {
        let mut args = dispatch_with_prompt(None);
        resolve_dispatch_prompt(&mut args).expect("no prompt is fine");
        assert!(args.prompt.is_none());
    }
}

/// Compile the one durable provider-cell plan used by local Cook and Lab handoff.
pub(crate) fn compile_cook_plan(
    args: &AgentTaskCookArgs,
    provision: Value,
) -> homeboy::core::Result<AgentTaskPlan> {
    let pending_lookup = matches!(
        provision.get("action").and_then(Value::as_str),
        Some("lookup_pending" | "attestation_pending" | "planned_create")
    );
    let workspace = (!pending_lookup)
        .then(|| {
            provision
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    if workspace.is_none() && !pending_lookup {
        return Err(homeboy::core::Error::internal_unexpected(
            "Cook destination provisioning did not return a task worktree path".to_string(),
        ));
    }
    let mut dispatch = dispatch_args_for_cook(args);
    resolve_dispatch_prompt(&mut dispatch)?;
    // Provisioning makes an explicit --cwd authoritative, otherwise this is the
    // resolved managed destination. Pass that exact linked worktree downstream.
    dispatch.cwd = None;
    dispatch.workspace = workspace.clone();
    validate_provider_evidence_inputs(&args.provider_evidence_inputs, dispatch.prompt.as_deref())?;
    rewrite_provider_evidence_prompt(
        &mut dispatch.prompt,
        &args.provider_evidence_inputs,
        workspace.as_deref(),
    );
    if let Some(workspace) = workspace.as_deref() {
        validate_cook_destination_identity(args, Path::new(workspace))?;
    }
    let mut request = dispatch_service::resolve_dispatch_request(dispatch.into())?;
    let mut plan = match dispatch_service::build_controller_dispatch_plan(&mut request) {
        Ok(plan) => plan,
        // A managed promotion handle may be intentionally unavailable until
        // after provider execution. Keep the compiled task durable and let
        // promotion report its established controlled policy failure.
        Err(error)
            if request.workspace.as_deref() == args.to_worktree.as_deref()
                && error.message.contains(
                    "neither an existing directory nor a resolvable managed worktree handle",
                ) =>
        {
            request.workspace = None;
            dispatch_service::build_controller_dispatch_plan(&mut request)?
        }
        Err(error) => return Err(error),
    };
    plan.options.candidate_completion = args.candidate_completion;
    record_cook_provision(&mut plan, provision);
    if let Some(identity) = &args.repository_identity {
        plan.metadata["cook_repository_identity"] = identity.clone();
    }
    for task in &mut plan.tasks {
        if pending_lookup {
            continue;
        }
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "workspace",
                "Cook requires a bound task worktree",
                None,
                None,
            )
        })?;
        task.metadata["cook_workspace_identity"] = workspace_identity_attestation(Path::new(root))?;
    }
    homeboy::agents::agent_task_provider::AgentTaskProviderCatalog::discover()
        .validate_explicit_models(&plan)?;
    record_cook_goal(&mut plan, args.goal.as_deref());
    if !args.provider_evidence_inputs.is_empty() {
        let workspace = workspace.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence cannot be projected until Cook has a resolved workspace path",
                None,
                None,
            )
        })?;
        let evidence = project_provider_evidence_inputs(
            &args.provider_evidence_inputs,
            Path::new(workspace),
            None,
        )?;
        for task in &mut plan.tasks {
            if !task.executor.config.is_object() {
                task.executor.config = serde_json::json!({});
            }
            task.executor.config["evidence_inputs"] = serde_json::to_value(&evidence)
                .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        }
    }
    Ok(plan)
}

pub(crate) fn validate_provider_evidence_inputs(
    inputs: &[AgentTaskProviderEvidenceInput],
    prompt: Option<&str>,
) -> homeboy::core::Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    let mut sources = std::collections::BTreeSet::new();
    for input in inputs {
        if input.id.trim().is_empty()
            || input.id.contains(['/', '\\'])
            || input.id == "."
            || input.id == ".."
        {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence ids must be non-empty path-free names",
                Some(input.id.clone()),
                None,
            ));
        }
        if !ids.insert(input.id.clone()) || !sources.insert(input.source.clone()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence ids and sources must be unique",
                Some(input.source.clone()),
                None,
            ));
        }
        let source = Path::new(&input.source);
        let metadata = std::fs::symlink_metadata(source).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence sources must be existing absolute regular files",
                Some(format!("{}: {error}", input.source)),
                None,
            )
        })?;
        if !source.is_absolute() || !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(homeboy::core::Error::validation_invalid_argument("provider-evidence", "provider evidence sources must be existing absolute regular files without symlinks", Some(input.source.clone()), None));
        }
    }
    if let Some(prompt) = prompt {
        for token in prompt.split_whitespace() {
            let path = token.trim_matches(|character: char| {
                matches!(
                    character,
                    '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
                )
            });
            if path.starts_with('/') && !sources.contains(path) {
                return Err(homeboy::core::Error::validation_invalid_argument("prompt", format!("prompt names undeclared absolute evidence path `{path}`"), Some(path.to_string()), Some(vec!["Declare it with --provider-evidence '{\"id\":\"evidence\",\"source\":\"/absolute/path\"}' so Homeboy projects it into the provider workspace.".to_string()])));
            }
        }
    }
    Ok(())
}

pub(crate) fn projected_provider_evidence(
    inputs: &[AgentTaskProviderEvidenceInput],
    workspace: Option<&str>,
) -> homeboy::core::Result<Vec<Value>> {
    let workspace = workspace.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence requires a bound Cook workspace",
            None,
            None,
        )
    })?;
    Ok(inputs.iter().map(|input| serde_json::json!({
        "id": input.id,
        "path": Path::new(workspace).join(".homeboy/evidence").join(&input.id).join(Path::new(&input.source).file_name().unwrap_or_default()).display().to_string(),
        "read_only": true,
    })).collect())
}

pub(crate) fn project_provider_evidence_inputs(
    inputs: &[AgentTaskProviderEvidenceInput],
    workspace: &Path,
    prompt: Option<&str>,
) -> homeboy::core::Result<Vec<Value>> {
    validate_provider_evidence_inputs(inputs, prompt)?;
    let mut projected = projected_provider_evidence(inputs, workspace.to_str())?;
    for (input, projection) in inputs.iter().zip(&mut projected) {
        let destination = PathBuf::from(projection["path"].as_str().expect("evidence path"));
        let (bytes, digest) =
            secure_provider_evidence_copy(Path::new(&input.source), &destination)?;
        projection["size_bytes"] = serde_json::json!(bytes);
        projection["sha256"] = serde_json::json!(digest);
    }
    Ok(projected)
}

#[cfg(unix)]
fn secure_provider_evidence_copy(
    source: &Path,
    destination: &Path,
) -> homeboy::core::Result<(u64, String)> {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    fn directory(path: &Path, create: bool) -> homeboy::core::Result<std::fs::File> {
        if !path.is_absolute() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "evidence paths must be absolute",
                Some(path.display().to_string()),
                None,
            ));
        }
        let root = std::ffi::CString::new("/").expect("root");
        let fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(homeboy::core::Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                None,
            ));
        }
        let mut current = unsafe { std::fs::File::from_raw_fd(fd) };
        for component in path.components().skip(1) {
            let name = std::ffi::CString::new(component.as_os_str().as_bytes()).map_err(|_| {
                homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "evidence path contains NUL",
                    None,
                    None,
                )
            })?;
            if create {
                let result = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
                if result != 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(homeboy::core::Error::internal_io(
                        std::io::Error::last_os_error().to_string(),
                        Some(path.display().to_string()),
                    ));
                }
            }
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "evidence paths cannot traverse symlink or non-directory components",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            current = unsafe { std::fs::File::from_raw_fd(fd) };
        }
        Ok(current)
    }
    let source_parent = directory(source.parent().expect("source parent"), false)?;
    let source_name = std::ffi::CString::new(source.file_name().expect("source name").as_bytes())
        .expect("source name");
    let source_fd = unsafe {
        libc::openat(
            source_parent.as_raw_fd(),
            source_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if source_fd < 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source could not be securely opened",
            Some(source.display().to_string()),
            None,
        ));
    }
    let input = unsafe { std::fs::File::from_raw_fd(source_fd) };
    let metadata = input.metadata().map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    if !metadata.is_file() || metadata.len() > MAX_PROVIDER_EVIDENCE_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence must be a bounded regular file",
            Some(source.display().to_string()),
            None,
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    input
        .take(MAX_PROVIDER_EVIDENCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(source.display().to_string()))
        })?;
    if bytes.len() as u64 > MAX_PROVIDER_EVIDENCE_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence exceeds the maximum size",
            Some(source.display().to_string()),
            None,
        ));
    }
    let parent = directory(destination.parent().expect("destination parent"), true)?;
    let final_name = std::ffi::CString::new(
        destination
            .file_name()
            .expect("destination name")
            .as_bytes(),
    )
    .expect("destination name");
    let temporary_name = std::ffi::CString::new(format!(".evidence-{}", uuid::Uuid::new_v4()))
        .expect("temporary name");
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o400,
        )
    };
    if fd < 0 {
        return Err(homeboy::core::Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some(destination.display().to_string()),
        ));
    }
    let mut output = unsafe { std::fs::File::from_raw_fd(fd) };
    output
        .write_all(&bytes)
        .and_then(|_| output.sync_all())
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        })?;
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            parent.as_raw_fd(),
            final_name.as_ptr(),
        )
    } != 0
    {
        return Err(homeboy::core::Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some(destination.display().to_string()),
        ));
    }
    Ok((
        bytes.len() as u64,
        format!(
            "sha256:{}",
            homeboy_engine_primitives::content_hash::sha256_hex(&bytes)
        ),
    ))
}

#[cfg(not(unix))]
fn secure_provider_evidence_copy(
    source: &Path,
    destination: &Path,
) -> homeboy::core::Result<(u64, String)> {
    let bytes = std::fs::read(source).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(source.display().to_string()))
    })?;
    if bytes.len() as u64 > MAX_PROVIDER_EVIDENCE_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence exceeds the maximum size",
            Some(source.display().to_string()),
            None,
        ));
    }
    std::fs::create_dir_all(destination.parent().expect("destination parent")).map_err(
        |error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        },
    )?;
    std::fs::write(destination, &bytes).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(destination.display().to_string()),
        )
    })?;
    Ok((
        bytes.len() as u64,
        format!(
            "sha256:{}",
            homeboy_engine_primitives::content_hash::sha256_hex(&bytes)
        ),
    ))
}

pub(crate) fn rewrite_provider_evidence_prompt(
    prompt: &mut Option<String>,
    inputs: &[AgentTaskProviderEvidenceInput],
    workspace: Option<&str>,
) {
    let Some(prompt) = prompt else { return };
    let Some(workspace) = workspace else { return };
    for input in inputs {
        let destination = Path::new(workspace)
            .join(".homeboy/evidence")
            .join(&input.id)
            .join(Path::new(&input.source).file_name().unwrap_or_default());
        *prompt = prompt.replace(&input.source, &destination.display().to_string());
    }
}

#[cfg(test)]
mod provider_evidence_tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn projects_declared_file_and_rewrites_prompt_to_workspace_evidence() {
        let temp = tempfile::tempdir().expect("temporary workspace");
        let source = temp.path().join("external.json");
        std::fs::write(&source, "{\"accepted\":true}").expect("write evidence");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");
        let source = source.canonicalize().expect("canonical source");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let input = AgentTaskProviderEvidenceInput {
            id: "acceptance".to_string(),
            source: source.display().to_string(),
        };

        let projected = project_provider_evidence_inputs(&[input.clone()], &workspace, None)
            .expect("project declared evidence");
        let path = PathBuf::from(projected[0]["path"].as_str().expect("projected path"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read projected evidence"),
            "{\"accepted\":true}"
        );
        assert!(std::fs::metadata(&path)
            .expect("evidence metadata")
            .permissions()
            .readonly());
        assert_eq!(
            projected[0]["sha256"],
            "sha256:11a49f853eb8befe94fef278d487125cd20930b9e41c4c0934394443e7f00878"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path)
                .expect("evidence mode")
                .permissions()
                .mode()
                & 0o777,
            0o400
        );

        let mut prompt = Some(format!("Read {} before editing.", source.display()));
        rewrite_provider_evidence_prompt(&mut prompt, &[input], workspace.to_str());
        assert_eq!(
            prompt.expect("rewritten prompt"),
            format!("Read {} before editing.", path.display())
        );
    }

    #[test]
    fn rejects_undeclared_absolute_prompt_path_before_provider_admission() {
        let error = validate_provider_evidence_inputs(&[], Some("Read /private/evidence.json."))
            .expect_err("undeclared prompt path is rejected");
        assert_eq!(error.details["field"], "prompt");
        assert!(error.message.contains("undeclared absolute evidence path"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_evidence_source() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("temporary workspace");
        let source = temp.path().join("source.json");
        let link = temp.path().join("source-link.json");
        std::fs::write(&source, "{}").expect("write source");
        symlink(&source, &link).expect("create source symlink");
        let error = project_provider_evidence_inputs(
            &[AgentTaskProviderEvidenceInput {
                id: "source".to_string(),
                source: link.display().to_string(),
            }],
            temp.path(),
            None,
        )
        .expect_err("symlink source rejected");
        assert!(error.message.contains("regular files"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_destination_ancestor_without_writing_through_it() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("temporary workspace");
        let source = temp.path().join("source.json");
        let outside = temp.path().join("outside");
        let workspace = temp.path().join("workspace");
        std::fs::write(&source, "{}").expect("write source");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::create_dir_all(workspace.join(".homeboy")).expect("create workspace");
        symlink(&outside, workspace.join(".homeboy/evidence")).expect("create destination symlink");
        let error = project_provider_evidence_inputs(
            &[AgentTaskProviderEvidenceInput {
                id: "source".to_string(),
                source: source
                    .canonicalize()
                    .expect("canonical source")
                    .display()
                    .to_string(),
            }],
            &workspace.canonicalize().expect("canonical workspace"),
            None,
        )
        .expect_err("symlink destination rejected");
        assert!(error.message.contains("symlink or non-directory"));
        assert!(!outside.join("source/source.json").exists());
    }
}

#[cfg(unix)]
fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(canonical.display().to_string()))
    })?;
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(&canonical)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let git_file = canonical.join(".git");
    let git_metadata = std::fs::symlink_metadata(&git_file).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let git_content = std::fs::read_to_string(&git_file).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let gitdir_target = git_content
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .and_then(|target| std::fs::canonicalize(canonical.join(target)).ok());
    Ok(
        serde_json::json!({ "canonical_path": canonical, "device": metadata.dev(), "inode": metadata.ino(), "git_dir": git_dir, "git_file_is_file": git_metadata.file_type().is_file(), "git_file_content": git_content, "gitdir_target": gitdir_target }),
    )
}

#[cfg(not(unix))]
fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    Ok(serde_json::json!({ "canonical_path": canonical }))
}

fn record_cook_goal(plan: &mut AgentTaskPlan, goal: Option<&str>) {
    let Some(goal) = goal else {
        return;
    };
    if !plan.metadata.is_object() {
        plan.metadata = serde_json::json!({});
    }
    plan.metadata["cook_goal"] = serde_json::json!(goal);
    for task in &mut plan.tasks {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["cook_goal"] = serde_json::json!(goal);
    }
}

fn git_head_sha(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn default_loop_title(args: &AgentTaskCookArgs) -> String {
    let target = args
        .dispatch
        .repo
        .as_deref()
        .or(args.dispatch.task_url.as_deref())
        .unwrap_or("agent task");
    format!("Cook {target}")
}

fn default_loop_commit_message(args: &AgentTaskCookArgs) -> String {
    let target = args.dispatch.repo.as_deref().unwrap_or("agent task");
    format!("fix: cook {target}")
}

pub(super) fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let mut plan = agent_task_service::read_plan(&args.plan)?;
    if let Some(timeout_ms) = args.timeout_ms {
        plan.options.timeout_ms = Some(timeout_ms);
    }
    // Managed services own durable process logs, so a direct plan invocation
    // needs a run identity even when the caller did not provide one.
    let record_run_id = args.record_run_id.or_else(|| {
        (!plan.services.is_empty()).then(|| format!("run-plan-{}", uuid::Uuid::new_v4()))
    });
    emit_runner_lifecycle_progress(&plan, record_run_id.as_deref());
    run_loaded_plan(
        plan,
        record_run_id.as_deref(),
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

fn emit_runner_lifecycle_progress(plan: &AgentTaskPlan, run_id: Option<&str>) {
    if std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV).is_none() {
        return;
    }
    for task in &plan.tasks {
        println!(
            "HOMEBOY_RUNNER_PROGRESS {}",
            serde_json::json!({
                "schema": "homeboy/runner-progress/v1",
                "phase": "provider_dispatch",
                "current_item": task.task_id,
                "metadata": {
                    "agent_task_run_id": run_id,
                    "task_id": task.task_id,
                    "provider": task.executor.backend,
                    "event": "provider_selected",
                },
            })
        );
    }
}

pub(super) fn run_loaded_plan(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    let result = agent_task_service::run_loaded_plan(plan, record_run_id, executor)?;
    let value =
        if std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV).is_some() {
            aggregate_value_with_failure_reasons(&result.value)
        } else {
            super::status::compact_aggregate_summary(&result.value, record_run_id)
        };
    Ok((value, result.exit_code))
}

pub(super) fn run_submitted(args: RunArgs) -> CmdResult<Value> {
    run_submitted_with_executor(
        args.run_id,
        args.timeout_ms,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

pub(super) fn run_submitted_with_executor(
    run_id: String,
    timeout_ms: Option<u64>,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    let result =
        agent_task_service::run_submitted_with_timeout(run_id.clone(), timeout_ms, executor)?;
    Ok((
        super::status::compact_aggregate_summary(&result.value, Some(&run_id)),
        result.exit_code,
    ))
}

pub(super) fn run_next(args: RunNextArgs) -> CmdResult<Value> {
    run_next_with_executor_and_fanout(
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
        args.fanout,
    )
}

pub(super) fn run_next_with_executor(executor: SharedAgentTaskExecutor) -> CmdResult<Value> {
    run_next_with_executor_and_fanout(executor, None)
}

pub(super) fn run_next_with_executor_and_fanout(
    executor: SharedAgentTaskExecutor,
    fanout_id: Option<String>,
) -> CmdResult<Value> {
    let scoped_run_ids = fanout_id
        .as_deref()
        .map(homeboy::agents::agent_tasks::batch::owned_child_run_ids)
        .transpose()?
        .map(|run_ids| run_ids.into_iter().collect::<HashSet<_>>());
    let result = agent_task_service::run_next_with_cook_dispatcher(
        executor,
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
        scoped_run_ids.as_ref(),
    )?;
    let Some(aggregate) = result.value else {
        return Ok((
            serde_json::json!({ "claimed": false, "queue_skips": result.skipped, "queue_admission": result.queue_admission }),
            0,
        ));
    };
    let mut value = aggregate_value_with_failure_reasons(&aggregate);
    if let Value::Object(object) = &mut value {
        object.insert(
            "queue_skips".to_string(),
            serde_json::to_value(result.skipped).unwrap_or(Value::Null),
        );
        object.insert(
            "queue_admission".to_string(),
            serde_json::to_value(result.queue_admission).unwrap_or(Value::Null),
        );
    }
    Ok((value, result.exit_code))
}

pub(super) fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let record = agent_task_service::submit_plan_spec(&args.plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

pub(super) fn resume(args: impl Into<LifecycleReadArgs>) -> CmdResult<Value> {
    let args = args.into();
    run_resume_with_executor_and_bridge(
        args.run_id,
        args.bridge,
        args.since_cursor,
        args.full,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

pub(super) fn run_resume_with_executor(
    run_id: String,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    run_resume_with_executor_and_bridge(run_id, false, None, false, executor)
}

pub(super) fn run_resume_with_executor_and_bridge(
    run_id: String,
    bridge: bool,
    since_cursor: Option<u64>,
    full: bool,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    let needs_transport_recovery =
        bridge && agent_task_service::terminal_transport_recovery_required(&run_id);
    if needs_transport_recovery {
        // Recover the authenticated terminal runner snapshot before `resume`
        // can short-circuit on a previously persisted lossy aggregate.
        agent_task_service::recover_terminal_transport_proxy_evidence(&run_id)?;
    }
    let result = agent_task_service::resume(run_id.clone(), executor)?;
    if bridge {
        // Resume first imports authoritative terminal runner evidence when the
        // local aggregate is absent. Reproject only after that shared recovery
        // contract has persisted the aggregate and identity.
        agent_task_service::reconcile_terminal_artifact_projection(&run_id)?;
        let status = agent_task_service::run_status(&run_id, since_cursor)?;
        return Ok((
            serde_json::to_value(status).unwrap_or(Value::Null),
            result.exit_code,
        ));
    }
    Ok((
        if full {
            aggregate_value_with_failure_reasons(&result.value)
        } else {
            super::status::compact_aggregate_summary(&result.value, Some(&run_id))
        },
        result.exit_code,
    ))
}

pub(super) fn retry(args: RetryArgs) -> CmdResult<Value> {
    retry_with(
        args,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )
}

pub(super) fn retry_with<F>(
    args: RetryArgs,
    executor: SharedAgentTaskExecutor,
    reconstruct_dispatcher: F,
) -> CmdResult<Value>
where
    F: Fn(
            &Value,
        ) -> homeboy::core::Result<
            Option<Arc<dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher>>,
        > + Copy,
{
    let result = agent_task_service::retry(
        &args.run_id,
        args.new_run_id.as_deref(),
        args.run,
        args.force,
    )?;
    if result.run {
        if result.record.metadata["cook_id"].is_string() {
            return continue_cook_with_queued_execution(
                CookContinueArgs {
                    cook_or_attempt_id: result.record.run_id,
                    preflight: false,
                    rearm: false,
                    artifact_id: None,
                    full: false,
                },
                executor,
                reconstruct_dispatcher,
                true,
            );
        }
        return run_submitted_with_executor(result.record.run_id, None, executor);
    }
    Ok((
        serde_json::to_value(result.record).unwrap_or(Value::Null),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        cook_attached_local_placement_disclosure, cook_continuation_status,
        cook_provider_timeout_disclosure, cook_report_with_continuation,
        cook_resolved_policy_disclosure, durable_cook_identity_lines, preflight_continue_cook,
    };
    use crate::commands::agent_task::args::CookContinueArgs;

    #[test]
    fn durable_cook_identity_block_leads_with_the_run_id_and_follow_up_commands() {
        let lines = durable_cook_identity_lines(Some("cook-10419"), "cook-10419-attempt-1");

        // The very first thing an interrupted caller sees must be the handle.
        assert!(
            lines[0].contains("cook-10419-attempt-1"),
            "identity block must lead with the durable run id: {lines:?}"
        );
        assert!(lines[0].contains("cook-10419"));
        let joined = lines.join("\n");
        assert!(joined.contains("homeboy --placement local agent-task status cook-10419-attempt-1"));
        assert!(joined.contains("homeboy --placement local agent-task logs cook-10419-attempt-1"));
        assert!(joined.contains("homeboy --placement local agent-task cancel cook-10419-attempt-1"));
    }

    #[test]
    fn durable_cook_identity_block_omits_a_cook_alias_equal_to_the_run_id() {
        let lines = durable_cook_identity_lines(Some("run-9163"), "run-9163");

        assert!(!lines[0].contains("(cook"), "no redundant alias: {lines:?}");
        assert!(lines[0].contains("run-9163"));
    }

    #[test]
    fn resolved_cook_policy_disclosure_reports_the_execution_budget() {
        let mut plan = homeboy::agents::agent_task_scheduler::AgentTaskPlan::new("plan", vec![]);
        plan.options.execution_budget =
            homeboy::agents::agent_task_scheduler::AgentTaskExecutionBudget::new(4, 1, 2);

        assert_eq!(
            cook_resolved_policy_disclosure(2, &plan),
            "cook: retry policy: 1 initial execution, 1 same-provider remediation retry(ies), 2 rotation(s), 4 provider execution(s) maximum"
        );
    }

    /// A budget nobody states is a budget an operator can only discover by
    /// losing a run to it (#12568), so the preamble reports the resolved value —
    /// the inherited default included.
    #[test]
    fn provider_timeout_disclosure_reports_the_resolved_budget() {
        let mut plan = homeboy::agents::agent_task_scheduler::AgentTaskPlan::new("plan", vec![]);

        assert_eq!(
            cook_provider_timeout_disclosure(&plan),
            format!(
                "cook: provider timeout: {}s per provider execution (override with --timeout-ms)",
                homeboy::agents::agent_task_timeout::DEFAULT_PROVIDER_TIMEOUT_MS / 1_000
            ),
            "the inherited default must be named, not left implicit"
        );

        plan.options.timeout_ms = Some(2_700_000);
        assert_eq!(
            cook_provider_timeout_disclosure(&plan),
            "cook: provider timeout: 2700s per provider execution (override with --timeout-ms)"
        );
    }

    /// The unsafe shape is the default one, so the submission preamble is the
    /// only place an operator learns that this Cook dies with its client.
    #[test]
    fn attached_local_placement_is_disclosed_at_submission() {
        assert_eq!(
            cook_attached_local_placement_disclosure(Some("local"), false).as_deref(),
            Some("cook: attached local placement — the provider runs in this client's process tree and will not survive it; pass --detach-after-handoff to re-execute the Cook in its own session")
        );
    }

    /// A detached local Cook already survives its client, and a Lab-placed
    /// provider never ran inside it. Warning there would be noise.
    #[test]
    fn a_detached_or_lab_placed_cook_is_not_warned_about_its_client() {
        assert_eq!(
            cook_attached_local_placement_disclosure(Some("local"), true),
            None
        );
        assert_eq!(
            cook_attached_local_placement_disclosure(Some("lab"), false),
            None
        );
        assert_eq!(cook_attached_local_placement_disclosure(None, false), None);
    }

    #[test]
    fn in_flight_cook_report_keeps_provider_state_and_managed_continuation_separate() {
        let report = cook_report_with_continuation(serde_json::json!({
            "cook_id": "cook-1",
            "latest_run_id": "cook-1-attempt-1",
            "status": "in_flight",
            "attempts": [{ "run_state": "Running" }],
        }));

        assert_eq!(report["provider"]["state"], "Running");
        assert_eq!(
            report["remaining_phases"],
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"])
        );
        assert_eq!(
            report["continuation_command"],
            "homeboy agent-task cook-continue cook-1-attempt-1"
        );
    }

    #[test]
    fn cook_continue_reports_runner_capacity_without_offering_a_duplicate_dispatch() {
        crate::test_support::with_isolated_home(|_| {
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![]);
            homeboy::agents::agent_tasks::lifecycle::submit_plan(
                &plan,
                Some("cook-queue-attempt-1"),
            )
            .expect("persist queued attempt");
            homeboy::agents::agent_tasks::lifecycle::rewrite_record_for_test(
                "cook-queue-attempt-1",
                |record| {
                    record.metadata = serde_json::json!({
                        "runner_id": "fixture-lab",
                        "runner_job_status": "queued",
                        "runner_queue": { "state": "waiting_for_capacity" },
                    });
                },
            )
            .expect("record accepted queue ownership");
            let record = homeboy::agents::agent_tasks::lifecycle::status("cook-queue-attempt-1")
                .expect("read queued attempt");

            let report = cook_continuation_status("cook-queue", &record);

            assert_eq!(report["status"], "accepted_unscheduled");
            assert_eq!(report["guidance"]["action"], "await_runner_capacity");
            assert_eq!(
                report["guidance"]["command"],
                "homeboy runner status fixture-lab"
            );
            assert!(report.get("continuation_command").is_none());
        });
    }

    #[test]
    fn cook_continue_reports_a_queued_record_without_runner_as_unscheduled() {
        crate::test_support::with_isolated_home(|_| {
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![]);
            homeboy::agents::agent_tasks::lifecycle::submit_plan(
                &plan,
                Some("cook-unassigned-attempt-1"),
            )
            .expect("persist queued attempt");
            let record =
                homeboy::agents::agent_tasks::lifecycle::status("cook-unassigned-attempt-1")
                    .expect("read queued attempt without a runner boundary");

            let report = cook_continuation_status("cook-unassigned", &record);

            assert_eq!(report["status"], "accepted_unscheduled");
            assert_eq!(report["guidance"]["action"], "schedule_queued_run");
            assert_eq!(
                report["guidance"]["command"],
                "homeboy agent-task run cook-unassigned-attempt-1"
            );
            assert!(report.get("continuation_command").is_none());
        });
    }

    #[test]
    fn cook_continue_reports_active_runner_work_with_a_watch_command() {
        crate::test_support::with_isolated_home(|_| {
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![]);
            homeboy::agents::agent_tasks::lifecycle::submit_plan(
                &plan,
                Some("cook-active-attempt-1"),
            )
            .expect("persist active attempt");
            homeboy::agents::agent_tasks::lifecycle::mark_running("cook-active-attempt-1")
                .expect("runner starts the provider attempt");
            homeboy::agents::agent_tasks::lifecycle::rewrite_record_for_test(
                "cook-active-attempt-1",
                |record| {
                    record.metadata = serde_json::json!({
                        "runner_id": "fixture-lab",
                        "runner_job_status": "running",
                        "provider_run_ids": ["fixture-provider-run-1"],
                        "phase": "provider_execution",
                    });
                },
            )
            .expect("record active runner and provider evidence");
            let record = homeboy::agents::agent_tasks::lifecycle::status("cook-active-attempt-1")
                .expect("read active runner-backed provider attempt");

            let report = cook_continuation_status("cook-active", &record);

            assert_eq!(report["status"], "observation_in_progress");
            assert_eq!(report["guidance"]["action"], "watch_provider");
            assert_eq!(
                report["guidance"]["command"],
                "homeboy agent-task logs cook-active-attempt-1"
            );
            assert!(report.get("continuation_command").is_none());
        });
    }

    #[test]
    fn cook_continue_preflight_reports_a_read_only_rejection_without_creating_a_run() {
        crate::test_support::with_isolated_home(|_| {
            let before = homeboy::agents::agent_tasks::lifecycle::list_records()
                .expect("read initial lifecycle records");
            let (report, exit_code) = preflight_continue_cook(CookContinueArgs {
                cook_or_attempt_id: "missing-cook".to_string(),
                preflight: true,
                rearm: false,
                artifact_id: None,
                full: false,
            })
            .expect("preflight returns a machine-readable rejection");
            let after = homeboy::agents::agent_tasks::lifecycle::list_records()
                .expect("read lifecycle records after preflight");

            assert_eq!(exit_code, 1);
            assert_eq!(report["admitted"], false);
            assert_eq!(report["phases"][0]["phase"], "recipe");
            assert_eq!(report["side_effects"]["provider_dispatch"], false);
            assert_eq!(report["side_effects"]["git_mutation"], false);
            assert_eq!(report["side_effects"]["github_mutation"], false);
            assert_eq!(report["side_effects"]["finalization"], false);
            assert_eq!(
                before.len(),
                after.len(),
                "preflight must not materialize a run"
            );
        });
    }
}
