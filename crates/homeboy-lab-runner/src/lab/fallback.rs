//! Local-run fallback and unsupported-command diagnostics for Lab offload.

use crate::lab_offload_metadata_with_workspace_mapping;
use homeboy_core::plan::HomeboyPlan;
use homeboy_core::Error;

use super::offload::{attach_offload_selection, LabOffloadOutcome, OffloadObservation};

/// The policy reason for staying on the controller. Dynamic diagnostics belong
/// to their originating check; the outcome shape and message live here only.
pub(super) enum LocalFallbackReason<'a> {
    CanonicalPlacement(&'a homeboy_lab_contract::ExecutionPlacementDecision),
    ManagedPlacement,
    NoContract,
    AutomaticOffloadDisabled,
    LocalOnly(&'a str),
    LocalOverride,
    NoDefaultRunner,
    RunnerUnavailable(&'a str),
    PreparationFailed(&'a str),
    CapabilityMissing(&'a str),
    DaemonUnavailable(&'a str),
    IdentityDrift(&'a str),
}

impl LocalFallbackReason<'_> {
    pub(super) fn diagnostic(&self) -> String {
        match self {
            Self::CanonicalPlacement(_) => "canonical_local_placement".into(),
            Self::ManagedPlacement => "runner placement already resolved".into(),
            Self::NoContract => "command has no Lab contract".into(),
            Self::AutomaticOffloadDisabled => "automatic Lab offload disabled".into(),
            Self::LocalOverride => "placement_local_override".into(),
            Self::NoDefaultRunner => "no_default_runner".into(),
            Self::RunnerUnavailable(detail) => format!("runner_unavailable: {detail}"),
            Self::IdentityDrift(detail) => format!("runner_identity_drift: {detail}"),
            Self::LocalOnly(detail)
            | Self::PreparationFailed(detail)
            | Self::CapabilityMissing(detail)
            | Self::DaemonUnavailable(detail) => (*detail).into(),
        }
    }
}

/// Evidence already collected by offload, without another outcome constructor.
#[derive(Default)]
pub(super) struct LocalFallbackContext<'a> {
    pub source: Option<&'a str>,
    pub runner_id: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub observation: Option<&'a OffloadObservation>,
    pub remote_cwd: Option<&'a str>,
    pub workspace_mapping: Option<&'a serde_json::Value>,
    pub extra_metadata: Option<serde_json::Value>,
}

/// Sole constructor for local Lab outcomes, including skips before selection.
pub(super) fn run_local(
    plan: HomeboyPlan,
    reason: LocalFallbackReason<'_>,
    context: LocalFallbackContext<'_>,
) -> LabOffloadOutcome {
    let plan = match reason {
        LocalFallbackReason::ManagedPlacement => {
            crate::lab_plan::disabled_select_runner_plan(plan, "runner placement already resolved")
        }
        LocalFallbackReason::NoContract => {
            crate::lab_plan::disabled_select_runner_plan(plan, "command has no Lab contract")
        }
        LocalFallbackReason::AutomaticOffloadDisabled => {
            crate::lab_plan::disabled_select_runner_plan(plan, "automatic Lab offload disabled")
        }
        _ => plan,
    };
    let diagnostic = reason.diagnostic();
    let fallback = matches!(
        reason,
        LocalFallbackReason::RunnerUnavailable(_)
            | LocalFallbackReason::PreparationFailed(_)
            | LocalFallbackReason::CapabilityMissing(_)
            | LocalFallbackReason::DaemonUnavailable(_)
            | LocalFallbackReason::IdentityDrift(_)
    );
    let metadata = match reason {
        LocalFallbackReason::CanonicalPlacement(decision) => Some(serde_json::json!({
            "status": "skipped",
            "execution_placement_decision": decision,
        })),
        LocalFallbackReason::ManagedPlacement
        | LocalFallbackReason::NoContract
        | LocalFallbackReason::AutomaticOffloadDisabled => None,
        _ => {
            let mut metadata = lab_offload_metadata_with_workspace_mapping(
                &plan,
                context.source.unwrap_or("automatic"),
                context.runner_id,
                context.mode,
                if fallback { "fallback" } else { "skipped" },
                context.remote_cwd,
                Some(&diagnostic),
                context.workspace_mapping,
            );
            if let Some(observation) = context.observation {
                attach_offload_selection(&mut metadata, observation);
            }
            if let Some(serde_json::Value::Object(extra)) = context.extra_metadata {
                metadata
                    .as_object_mut()
                    .expect("offload metadata object")
                    .extend(extra);
            }
            Some(metadata)
        }
    };
    LabOffloadOutcome::RunLocal {
        metadata,
        plan,
        messages: if fallback {
            vec![format!("Lab offload: {diagnostic}; running locally.")]
        } else {
            Vec::new()
        },
    }
}

pub(super) fn local_execution_denied_error(reason: &str, runner_id: Option<&str>) -> Error {
    let mut hints = vec![
        "Use --runner <runner-id> to offload to Lab.".to_string(),
        "Use --placement local only when local execution on this controller is intentional."
            .to_string(),
    ];
    if let Some(runner_id) = runner_id {
        hints.insert(
            0,
            format!("Reconnect or repair runner `{runner_id}` before retrying."),
        );
    }
    Error::validation_invalid_argument(
        "placement_lab",
        format!("Lab placement refused local execution: {reason}"),
        runner_id.map(str::to_string),
        Some(hints),
    )
}

pub(super) fn is_build_command(args: &[String]) -> bool {
    args.get(1).is_some_and(|arg| arg == "build")
}

fn build_lab_replacement_hints(runner_id: Option<&str>) -> Vec<String> {
    let runner = runner_id.unwrap_or("<runner-id>");
    vec![
        format!(
            "Materialize the build workspace first: homeboy runner workspace sync {runner} --path <local-worktree> --mode snapshot"
        ),
        format!(
            "Then run the build in the returned runner_path: homeboy runner exec --cwd <runner_path> {runner} -- homeboy build <component>"
        ),
    ]
}

pub(super) fn unsupported_build_lab_error(field: &'static str, runner_id: Option<&str>) -> Error {
    Error::validation_invalid_argument(
        field,
        "homeboy build is not Lab-portable yet; it requires an explicit runner workspace sync and runner exec handoff instead of --runner/--placement lab on build."
            .to_string(),
        runner_id.map(str::to_string),
        Some(build_lab_replacement_hints(runner_id)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_skips_preserve_their_metadata_contract() {
        for (reason, expected_reason) in [
            (LocalFallbackReason::ManagedPlacement, None),
            (LocalFallbackReason::NoContract, None),
            (LocalFallbackReason::AutomaticOffloadDisabled, None),
            (
                LocalFallbackReason::LocalOnly("controller-owned"),
                Some("controller-owned"),
            ),
            (
                LocalFallbackReason::LocalOverride,
                Some("placement_local_override"),
            ),
            (
                LocalFallbackReason::NoDefaultRunner,
                Some("no_default_runner"),
            ),
        ] {
            let outcome = run_local(
                crate::lab_plan::base_lab_plan(None),
                reason,
                LocalFallbackContext::default(),
            );
            let LabOffloadOutcome::RunLocal {
                metadata, messages, ..
            } = outcome
            else {
                panic!("expected local outcome");
            };
            assert!(messages.is_empty());
            if let Some(reason) = expected_reason {
                let metadata = metadata.expect("skip metadata");
                assert_eq!(metadata["status"], "skipped");
                assert_eq!(metadata["fallback_reason"], reason);
            } else {
                assert!(metadata.is_none());
            }
        }
    }
}
