//! The one analysis of a failed release.
//!
//! A release result is walked here, once, for the failing step, its cause, and
//! the actions that repair or reproduce it. The bounded JSON projection
//! serializes this type as its `failure` block and the command envelope renders
//! its failure digest from it, so the compact and full outputs can never
//! disagree about what failed or what to run next.

use homeboy::core::error::{ExecutableAction, ACTIONS_DETAILS_KEY};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::response::{CommandNextAction, CommandNextActionKind};

const UNREPORTED_CAUSE: &str = "release step failed without a reported error";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseFailure {
    pub step: String,
    #[serde(rename = "type")]
    pub step_type: String,
    pub cause: String,
    #[serde(default)]
    pub next_actions: Vec<CommandNextAction>,
}

impl ReleaseFailure {
    /// Analyse a release result (`payload.result`) for its first failed or
    /// missing step. Returns `None` when no step failed.
    pub fn from_release_result(result: &Value) -> Option<Self> {
        let steps = result.get("run")?.get("result")?.get("steps")?.as_array()?;
        let failed_step = steps.iter().find(|step| {
            matches!(
                step.get("status").and_then(Value::as_str),
                Some("failed") | Some("missing")
            )
        })?;
        let component_id = result
            .get("component_id")
            .and_then(Value::as_str)
            .unwrap_or("<component>");
        let step = failed_step
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let step_type = failed_step
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(step);
        let first = |key: &str| {
            failed_step
                .get(key)
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
        };
        let cause = failed_step
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| first("missing"))
            .or_else(|| first("warnings"))
            .unwrap_or(UNREPORTED_CAUSE);

        // The step already classified itself (`reason: "gh-upload-failed"`) and
        // already computed the repair command. Dropping both is what left CI
        // printing `Release failed: Unknown error` while holding the exact fix
        // (#10441), so lift the classification into the cause and the repair
        // commands into next_actions.
        let step_data = failed_step.get("data");
        let cause = match step_data
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str)
            .filter(|reason| !reason.trim().is_empty())
        {
            Some(reason) => format!("{reason} — {cause}"),
            None => cause.to_string(),
        };

        let mut next_actions = error_actions(step_data);
        next_actions.extend(repair_actions(step_data));
        if next_actions.is_empty() {
            next_actions.push(gate_repro_action(step, step_type, component_id));
        }

        Some(Self {
            step: step.to_string(),
            step_type: step_type.to_string(),
            cause,
            next_actions,
        })
    }

    /// The one-line failure summary every renderer shows.
    pub fn summary(&self) -> String {
        format!(
            "Release step {} ({}) failed: {}",
            self.step,
            self.step_type,
            homeboy_engine_primitives::text::truncate_chars(&self.cause, 1000, "...")
        )
    }
}

/// Preserve explicit typed-error actions emitted by a release step. These are
/// more specific than the generic gate fallback and may declare that a command
/// is a fresh diagnostic rather than an exact reproduction.
fn error_actions(step_data: Option<&Value>) -> Vec<CommandNextAction> {
    step_data
        .and_then(|data| data.get("error_details"))
        .and_then(|details| details.get(ACTIONS_DETAILS_KEY))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<ExecutableAction>(value.clone()).ok())
        .map(CommandNextAction::from_action)
        .collect()
}

/// Lift a failed release step's own `repair` block into executable next
/// actions.
///
/// A publish failure is not a quality-gate failure: re-running a gate does
/// nothing for it, and the plan-only `--dry-run` fallback actively misleads.
/// The step already knows whether the GitHub Release object exists, which
/// decides the repair: an existing Draft must be *finished* (upload + publish),
/// never re-created, or the release ends up duplicated.
fn repair_actions(step_data: Option<&Value>) -> Vec<CommandNextAction> {
    let Some(repair) = step_data
        .and_then(|data| data.get("repair"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };

    let command = |key: &str| {
        repair
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let repair = |label: &str, command: &str| {
        CommandNextAction::new(label, command).with_kind(CommandNextActionKind::Repair)
    };

    let release_created = step_data
        .and_then(|data| data.get("release_created"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut actions = Vec::new();

    if release_created {
        if let Some(upload) = command("upload_command") {
            actions.push(repair(
                "attach the built artifacts to the release that already exists",
                upload,
            ));
        }
        if let Some(publish) = command("publish_command") {
            actions.push(repair(
                "publish that release once its assets verify",
                publish,
            ));
        }
    } else if let Some(create) = command("create_command") {
        actions.push(repair(
            "create the GitHub Release from the pushed tag and built artifacts (no new tag)",
            create,
        ));
    }

    if let Some(view) = command("view_command").filter(|_| !actions.is_empty()) {
        actions.push(repair("verify the release and its assets", view));
    }

    actions
}

/// Map a failed release step to a non-mutating command that actually executes
/// that gate.
///
/// `release --dry-run` only renders a plan: it reports quality gates as `ready`
/// without running them, so recommending it after a gate failure returns
/// success while the blocker is untouched (#10114). Point each gate at the
/// surface that can reproduce it instead, and when no such surface exists say
/// plainly that the fallback only inspects the plan.
fn gate_repro_action(step_id: &str, step_type: &str, component_id: &str) -> CommandNextAction {
    let gate = |value: &str| {
        value
            .strip_prefix("preflight.")
            .unwrap_or(value)
            .to_string()
    };
    let gate_name = {
        let by_id = gate(step_id);
        if by_id == step_id && step_type != step_id {
            gate(step_type)
        } else {
            by_id
        }
    };

    let review_gate = match gate_name.as_str() {
        "lint" => Some("lint"),
        "test" => Some("test"),
        // Package and build-structure validation are reproduced by the local
        // build quality gate rather than by re-running the release.
        "package" | "build" => Some("build"),
        _ => None,
    };

    match review_gate {
        Some(gate) => CommandNextAction::new(
            format!("reproduce the failed {gate} gate"),
            format!("homeboy review {gate} {component_id}"),
        ),
        None => CommandNextAction::new(
            "inspect release plan (plan only — does not run quality gates)",
            format!("homeboy release {component_id} --dry-run"),
        ),
    }
    .with_kind(CommandNextActionKind::Repair)
}
