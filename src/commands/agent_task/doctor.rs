//! `agent-task doctor`: the single preflight/repair path for cook readiness.
//!
//! Operators previously had to hand-chain several repair/update/diagnostic
//! commands before an `agent-task cook` could even queue: update extensions
//! locally and on the runner, verify `agent-task providers`, reason about the
//! configured vs bare runner Homeboy binary, reconnect a stale Lab daemon, and
//! retry. This handler runs that whole readiness chain in sequence, reusing the
//! existing provider-contract and runner-doctor logic, and returns a single
//! ready/blocked verdict with one issue-shaped blocker when a cook would not be
//! safe to queue (#4864).
//!
//! Stage 1 verifies extension-declared executor providers, the selected
//! backend/selector mapping, and provider secret readiness. Stage 2 reuses
//! `runner doctor --scope lab-offload [--repair]` to verify controller binary,
//! runner configured/bare binary, active daemon compatibility, installed
//! extension revisions, managed runner sources, and provider runner readiness —
//! applying safe repairs (daemon reconnect) when `--repair` is set. The handler
//! stays runtime-agnostic: every provider/backend fact comes from
//! extension-declared contracts, never hard-coded ecosystem names.

use serde_json::{json, Value};

use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;

use super::super::runner::doctor::{
    self, RunnerDoctorOptions, RunnerDoctorScope, RunnerDoctorStatus,
};
use super::super::CmdResult;
use super::args::AgentTaskDoctorArgs;

/// Run the cook readiness repair chain and return a combined verdict.
pub(crate) fn doctor(args: AgentTaskDoctorArgs) -> CmdResult<Value> {
    let provider_stage = provider_stage(&args);
    let runner_stage = runner_stage(&args)?;

    let verdict = combine_verdict(&provider_stage, &runner_stage);
    let exit_code = if verdict.ready { 0 } else { 1 };

    Ok((
        json!({
            "schema": "homeboy/agent-task-doctor/v1",
            "command": "agent-task.doctor",
            "runner": args.runner,
            "ready": verdict.ready,
            "verdict": verdict.summary,
            "blocker": verdict.blocker,
            "repair_requested": args.repair,
            "stages": {
                "provider_contracts": provider_stage.value,
                "runner_readiness": runner_stage,
            },
        }),
        exit_code,
    ))
}

/// Stage 1: extension-declared providers, selected backend/selector mapping,
/// and provider secret readiness. Mirrors `agent-task providers` so the cook's
/// provider contract surface is verified from the same source of truth.
struct ProviderStage {
    ready: bool,
    blocker: Option<Value>,
    value: Value,
}

fn provider_stage(args: &AgentTaskDoctorArgs) -> ProviderStage {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    let providers = executor.providers();
    let fallback_sources =
        homeboy::core::agent_tasks::provider::provider_secret_sources_for_providers(providers);
    let secret_env = homeboy::core::agent_tasks::secrets::secret_env_status_with_fallbacks(
        &args.secret_env,
        &fallback_sources,
    );

    // Resolve the backend the cook would use: explicit --backend, else the
    // extension/policy-declared default. Selector is operator-supplied only.
    let default_backend =
        homeboy::core::agent_tasks::provider::default_backend().unwrap_or_default();
    let selected_backend = args.backend.clone().or_else(|| default_backend.clone());

    // Map the selected backend (and optional selector) onto a declared provider
    // so we can confirm the cook has a concrete provider to dispatch to.
    let backend_match = selected_backend.as_deref().map(|backend| {
        let candidates: Vec<&_> = providers
            .iter()
            .filter(|provider| provider.backend == backend)
            .collect();
        let selected = match args.selector.as_deref() {
            Some(selector) => candidates
                .iter()
                .find(|provider| provider.id == selector)
                .copied(),
            None => candidates.first().copied(),
        };
        (candidates.len(), selected)
    });

    let mut blocker = None;
    let provider_ready;
    let mapping_value;

    match (selected_backend.as_deref(), backend_match) {
        (None, _) => {
            provider_ready = false;
            blocker = Some(json!({
                "stage": "provider_contracts",
                "code": "backend_unresolved",
                "message": "No executor backend was selected and no default backend is configured",
                "remediation": "Pass --backend or configure a default coding backend before cooking",
            }));
            mapping_value = json!({ "selected_backend": Value::Null });
        }
        (Some(backend), Some((count, selected))) => {
            if let Some(provider) = selected {
                provider_ready = true;
                mapping_value = json!({
                    "selected_backend": backend,
                    "selector": args.selector,
                    "provider_id": provider.id,
                    "provider_label": provider.label,
                    "candidate_count": count,
                    "default_backend": default_backend,
                });
            } else {
                provider_ready = false;
                blocker = Some(json!({
                    "stage": "provider_contracts",
                    "code": "selector_unmatched",
                    "message": format!(
                        "No provider for backend `{backend}` matched selector `{}`",
                        args.selector.as_deref().unwrap_or("")
                    ),
                    "remediation": "List providers with `homeboy agent-task providers` and pass a --selector that matches a declared provider id",
                }));
                mapping_value = json!({
                    "selected_backend": backend,
                    "selector": args.selector,
                    "candidate_count": count,
                    "default_backend": default_backend,
                });
            }
        }
        (Some(backend), None) => {
            // Unreachable in practice (Some backend always builds a match), but
            // keep the arm total and issue-shaped rather than panicking.
            provider_ready = false;
            blocker = Some(json!({
                "stage": "provider_contracts",
                "code": "backend_unmapped",
                "message": format!("Backend `{backend}` could not be mapped to a declared provider"),
                "remediation": "Verify provider contracts with `homeboy agent-task providers`",
            }));
            mapping_value = json!({
                "selected_backend": backend,
                "default_backend": default_backend,
            });
        }
    }

    let value = json!({
        "schema": "homeboy/agent-task-providers/v1",
        "ready": provider_ready,
        "capability_contract": homeboy::core::agent_tasks::provider::provider_capability_contract(),
        "providers": providers,
        "backend_mapping": mapping_value,
        "secret_env": secret_env,
    });

    ProviderStage {
        ready: provider_ready,
        blocker,
        value,
    }
}

/// Stage 2: runner readiness/repair. Reuses `runner doctor --scope lab-offload`
/// so the controller/runner binary, active daemon, installed extension
/// revisions, managed runner sources, and provider runner readiness are checked
/// (and safely repaired when `--repair` is set) without reimplementing any of
/// that logic here.
fn runner_stage(args: &AgentTaskDoctorArgs) -> homeboy::core::Result<Value> {
    let (report, _exit) = doctor::run_with_options(
        &args.runner,
        RunnerDoctorOptions {
            path: args.path.clone(),
            extensions: args.extensions.clone(),
            required_tools: args.required_tools.clone(),
            scope: RunnerDoctorScope::LabOffload,
            repair: args.repair,
        },
    )?;
    serde_json::to_value(report)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

struct CombinedVerdict {
    ready: bool,
    summary: String,
    blocker: Option<Value>,
}

/// Collapse the provider and runner stages into a single cook-readiness verdict.
/// "Ready" requires the provider stage to be ready and the runner readiness to
/// be free of error-level checks; runner warnings do not block a cook but are
/// preserved in the runner stage output for the operator. When not ready, the
/// first stage to fail supplies the one issue-shaped blocker.
fn combine_verdict(provider: &ProviderStage, runner: &Value) -> CombinedVerdict {
    let runner_status = runner
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let runner_ready = runner_status != serde_runner_status(RunnerDoctorStatus::Error);

    if !provider.ready {
        return CombinedVerdict {
            ready: false,
            summary: "Cook is blocked: provider contract readiness failed".to_string(),
            blocker: provider.blocker.clone(),
        };
    }

    if !runner_ready {
        let blocker = first_runner_error(runner).unwrap_or_else(|| {
            json!({
                "stage": "runner_readiness",
                "code": "runner_not_ready",
                "message": "Runner readiness reported an error-level check",
                "remediation": "Inspect the runner_readiness stage checks and rerun with --repair where safe",
            })
        });
        return CombinedVerdict {
            ready: false,
            summary: "Cook is blocked: runner readiness failed".to_string(),
            blocker: Some(blocker),
        };
    }

    let summary = if runner_status == serde_runner_status(RunnerDoctorStatus::Warning) {
        "Cook is ready: provider contracts and runner readiness passed (with non-blocking warnings)"
            .to_string()
    } else {
        "Cook is ready: provider contracts and runner readiness passed".to_string()
    };

    CombinedVerdict {
        ready: true,
        summary,
        blocker: None,
    }
}

/// Map the first error-level runner check into an issue-shaped blocker so the
/// operator gets one precise, actionable reason a cook cannot queue.
fn first_runner_error(runner: &Value) -> Option<Value> {
    let error_label = serde_runner_status(RunnerDoctorStatus::Error);
    runner
        .get("checks")
        .and_then(Value::as_array)?
        .iter()
        .find(|check| check.get("status").and_then(Value::as_str) == Some(error_label))
        .map(|check| {
            json!({
                "stage": "runner_readiness",
                "code": check.get("id").cloned().unwrap_or(Value::Null),
                "message": check.get("message").cloned().unwrap_or(Value::Null),
                "remediation": check.get("remediation").cloned().unwrap_or(Value::Null),
                "details": check.get("details").cloned().unwrap_or(Value::Null),
            })
        })
}

/// Serialize a `RunnerDoctorStatus` to its wire label so verdict comparisons use
/// the same `snake_case` strings the report emits, with no duplicated literals.
fn serde_runner_status(status: RunnerDoctorStatus) -> &'static str {
    match status {
        RunnerDoctorStatus::Ok => "ok",
        RunnerDoctorStatus::Warning => "warn",
        RunnerDoctorStatus::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_provider() -> ProviderStage {
        ProviderStage {
            ready: true,
            blocker: None,
            value: json!({ "ready": true }),
        }
    }

    fn blocked_provider() -> ProviderStage {
        ProviderStage {
            ready: false,
            blocker: Some(json!({
                "stage": "provider_contracts",
                "code": "backend_unresolved",
                "message": "no backend",
            })),
            value: json!({ "ready": false }),
        }
    }

    #[test]
    fn serde_runner_status_matches_report_wire_labels() {
        // The report serializes RunnerDoctorStatus as snake_case; verdict
        // comparisons must use the exact same strings.
        assert_eq!(serde_runner_status(RunnerDoctorStatus::Ok), "ok");
        assert_eq!(serde_runner_status(RunnerDoctorStatus::Warning), "warn");
        assert_eq!(serde_runner_status(RunnerDoctorStatus::Error), "error");
    }

    #[test]
    fn provider_failure_blocks_cook_with_its_own_blocker() {
        let provider = blocked_provider();
        let runner = json!({ "status": "ok", "checks": [] });
        let verdict = combine_verdict(&provider, &runner);
        assert!(!verdict.ready);
        assert_eq!(
            verdict.blocker.as_ref().unwrap()["code"],
            json!("backend_unresolved")
        );
        assert!(verdict.summary.contains("provider contract"));
    }

    #[test]
    fn runner_error_blocks_cook_with_first_error_check() {
        let provider = ready_provider();
        let runner = json!({
            "status": "error",
            "checks": [
                { "id": "homeboy", "status": "ok", "message": "fine" },
                {
                    "id": "daemon.exec",
                    "status": "error",
                    "message": "daemon unreachable",
                    "remediation": "reconnect",
                    "details": { "url": "http://x" }
                },
                {
                    "id": "lab.managed_source.x",
                    "status": "error",
                    "message": "later error"
                }
            ]
        });
        let verdict = combine_verdict(&provider, &runner);
        assert!(!verdict.ready);
        let blocker = verdict.blocker.unwrap();
        assert_eq!(blocker["stage"], json!("runner_readiness"));
        assert_eq!(blocker["code"], json!("daemon.exec"));
        assert_eq!(blocker["message"], json!("daemon unreachable"));
        assert_eq!(blocker["remediation"], json!("reconnect"));
    }

    #[test]
    fn ready_when_both_stages_pass() {
        let provider = ready_provider();
        let runner = json!({ "status": "ok", "checks": [] });
        let verdict = combine_verdict(&provider, &runner);
        assert!(verdict.ready);
        assert!(verdict.blocker.is_none());
        assert!(verdict.summary.contains("ready"));
        assert!(!verdict.summary.contains("warning"));
    }

    #[test]
    fn runner_warnings_do_not_block_cook_but_are_surfaced_in_verdict() {
        let provider = ready_provider();
        let runner = json!({
            "status": "warn",
            "checks": [
                { "id": "homeboy.version_skew", "status": "warn", "message": "skew" }
            ]
        });
        let verdict = combine_verdict(&provider, &runner);
        assert!(verdict.ready);
        assert!(verdict.blocker.is_none());
        assert!(verdict.summary.contains("non-blocking warnings"));
    }

    #[test]
    fn provider_stage_takes_precedence_over_runner_error() {
        // When both stages fail, the provider blocker is reported first so the
        // operator fixes contract readiness before chasing runner state.
        let provider = blocked_provider();
        let runner = json!({
            "status": "error",
            "checks": [
                { "id": "daemon.exec", "status": "error", "message": "x" }
            ]
        });
        let verdict = combine_verdict(&provider, &runner);
        assert!(!verdict.ready);
        assert_eq!(
            verdict.blocker.unwrap()["stage"],
            json!("provider_contracts")
        );
    }
}
