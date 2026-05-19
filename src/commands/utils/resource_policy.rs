use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::cli_surface::Commands;

use crate::commands::doctor::resources::{DoctorOutput, ResourceRecommendation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotCommand {
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePolicyWarning {
    pub command: &'static str,
    pub recommendation: ResourceRecommendation,
    pub message: String,
}

/// Persisted, host-pressure context captured at preflight time for any
/// observation run that originates from a "hot" command (`bench`, `rig up`,
/// `lint`, `test`, `audit`, etc.).
///
/// Generic across components and rigs. Persisted into observation run
/// `metadata_json` under the `resource_policy` key so later readers can
/// distinguish noisy timings caused by host pressure from regressions in the
/// system under test.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourcePolicyContext {
    /// Hot command label (e.g. `bench`, `lint`).
    pub command: String,
    /// Overall severity: `ok`, `warm`, or `hot`.
    pub severity: String,
    /// Whether the user passed `--force-hot` to intentionally bypass the
    /// warning. When true and severity is non-ok, the warning was suppressed
    /// from stderr but still recorded here.
    pub force_hot: bool,
    /// Whether a warning was emitted (or would have been) for this run.
    pub warned: bool,
    /// Human-readable warning message produced by the policy, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Structured host snapshot used to derive the severity.
    pub host: ResourcePolicyHostSnapshot,
}

/// Subset of the doctor resource report that explains why the resource policy
/// fired. Stored as plain numbers/strings so it round-trips cleanly through
/// JSON without depending on internal `DoctorOutput` types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourcePolicyHostSnapshot {
    /// Severity of the load average alone.
    pub load_severity: String,
    /// 1-minute load average if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_one: Option<f64>,
    /// 5-minute load average if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_five: Option<f64>,
    /// 15-minute load average if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_fifteen: Option<f64>,
    /// CPU count reported by the host.
    pub cpu_count: usize,
    /// Memory severity if memory data was collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_severity: Option<String>,
    /// Memory used percent if memory data was collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_used_percent: Option<f64>,
    /// Memory available in MB if memory data was collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_available_mb: Option<u64>,
    /// Number of Homeboy-adjacent processes already active.
    pub relevant_process_count: usize,
    /// Severity classification of the active process set.
    pub process_severity: String,
    /// Number of active rig run leases.
    pub active_rig_lease_count: usize,
    /// Severity classification of the active rig lease set.
    pub rig_lease_severity: String,
}

impl ResourcePolicyContext {
    /// Build a structured context from a `DoctorOutput`, the matched hot
    /// command, an optional warning, and whether `--force-hot` was passed.
    pub fn from_evaluation(
        command: HotCommand,
        resources: &DoctorOutput,
        warning: Option<&ResourcePolicyWarning>,
        force_hot: bool,
    ) -> Self {
        Self {
            command: command.label.to_string(),
            severity: severity_str(resources.recommendation).to_string(),
            force_hot,
            warned: warning.is_some(),
            message: warning.map(|warning| warning.message.clone()),
            host: ResourcePolicyHostSnapshot {
                load_severity: severity_str(resources.load.recommendation).to_string(),
                load_one: resources.load.one,
                load_five: resources.load.five,
                load_fifteen: resources.load.fifteen,
                cpu_count: resources.load.cpu_count,
                memory_severity: resources
                    .memory
                    .as_ref()
                    .map(|memory| severity_str(memory.recommendation).to_string()),
                memory_used_percent: resources.memory.as_ref().map(|memory| memory.used_percent),
                memory_available_mb: resources.memory.as_ref().map(|memory| memory.available_mb),
                relevant_process_count: resources.processes.relevant_count,
                process_severity: severity_str(resources.processes.recommendation).to_string(),
                active_rig_lease_count: resources.rig_leases.active_count,
                rig_lease_severity: severity_str(resources.rig_leases.recommendation).to_string(),
            },
        }
    }

    /// Serialize as the JSON value that lands inside observation
    /// `metadata_json["resource_policy"]`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

fn severity_str(recommendation: ResourceRecommendation) -> &'static str {
    match recommendation {
        ResourceRecommendation::Ok => "ok",
        ResourceRecommendation::Warm => "warm",
        ResourceRecommendation::Hot => "hot",
    }
}

/// Process-wide capture of the preflight resource policy decision so that
/// observation runs started later in the command can include it in their
/// metadata without re-querying the host (which would re-introduce flakiness
/// and double-count Homeboy as a relevant process).
fn captured_storage() -> &'static RwLock<Option<ResourcePolicyContext>> {
    static STORAGE: std::sync::OnceLock<RwLock<Option<ResourcePolicyContext>>> =
        std::sync::OnceLock::new();
    STORAGE.get_or_init(|| RwLock::new(None))
}

/// Capture the resource policy context for the current process. Idempotent
/// against later overwrites in production: once a context is recorded, repeat
/// captures are dropped so the preflight decision wins. Tests can override
/// this by calling [`reset_captured_context_for_test`] first.
pub fn capture_context(context: ResourcePolicyContext) {
    let mut slot = match captured_storage().write() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    if slot.is_none() {
        *slot = Some(context);
    }
}

/// Return a clone of the resource policy context captured at preflight time,
/// if any.
pub fn captured_context() -> Option<ResourcePolicyContext> {
    captured_storage().read().ok().and_then(|slot| slot.clone())
}

/// Clear the captured context. Test-only: production code never resets the
/// preflight decision so that the persisted observation matches the warning
/// the user actually saw on stderr.
#[cfg(test)]
pub fn reset_captured_context_for_test() {
    if let Ok(mut slot) = captured_storage().write() {
        *slot = None;
    }
}

pub fn hot_command(command: &Commands) -> Option<HotCommand> {
    match command {
        Commands::Bench(args) if args.is_run_command() => Some(HotCommand { label: "bench" }),
        Commands::Rig(args) if args.is_hot_resource_command() => {
            Some(HotCommand { label: "rig up" })
        }
        Commands::Fleet(args) if args.is_hot_resource_command() => Some(HotCommand {
            label: "fleet exec",
        }),
        Commands::Audit(args) if args.changed_since.is_none() && !args.conventions => {
            Some(HotCommand { label: "audit" })
        }
        Commands::Lint(args) if args.is_full_workspace_run() => Some(HotCommand { label: "lint" }),
        Commands::Test(args) if args.changed_since.is_none() => Some(HotCommand { label: "test" }),
        _ => None,
    }
}

pub fn evaluate(command: HotCommand, resources: &DoctorOutput) -> Option<ResourcePolicyWarning> {
    match resources.recommendation {
        ResourceRecommendation::Ok => None,
        recommendation => Some(ResourcePolicyWarning {
            command: command.label,
            recommendation,
            message: warning_message(command.label, recommendation, resources),
        }),
    }
}

fn warning_message(
    command: &'static str,
    recommendation: ResourceRecommendation,
    resources: &DoctorOutput,
) -> String {
    let severity = severity_str(recommendation);
    let reason = primary_reason(resources);
    format!(
        "Resource policy warning: machine is {severity}; starting `{command}` may skew results or add pressure. {reason} Use --runner <id> to offload this hot command to a connected Homeboy Lab runner, or use --force-hot to run locally without this warning."
    )
}

fn primary_reason(resources: &DoctorOutput) -> String {
    if resources.load.recommendation == ResourceRecommendation::Hot
        || resources.load.recommendation == ResourceRecommendation::Warm
    {
        if let Some(one) = resources.load.one {
            return format!(
                "Load average is {one:.1} across {} CPU(s).",
                resources.load.cpu_count
            );
        }
        return "Load average is elevated.".to_string();
    }

    if let Some(memory) = &resources.memory {
        if memory.recommendation == ResourceRecommendation::Hot
            || memory.recommendation == ResourceRecommendation::Warm
        {
            return format!(
                "Memory is {:.1}% used ({} MB available).",
                memory.used_percent, memory.available_mb
            );
        }
    }

    if resources.processes.recommendation == ResourceRecommendation::Hot
        || resources.processes.recommendation == ResourceRecommendation::Warm
    {
        return format!(
            "{} relevant Homeboy-adjacent process(es) are already active.",
            resources.processes.relevant_count
        );
    }

    if resources.rig_leases.recommendation == ResourceRecommendation::Hot
        || resources.rig_leases.recommendation == ResourceRecommendation::Warm
    {
        return format!(
            "{} rig run lease(s) are already active.",
            resources.rig_leases.active_count
        );
    }

    "Run `homeboy doctor resources` for details.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::resources::{
        LoadSummary, MemorySummary, ProcessSummary, RigLeaseSummary,
    };

    fn resources(recommendation: ResourceRecommendation) -> DoctorOutput {
        DoctorOutput {
            command: "doctor.resources",
            recommendation,
            load: LoadSummary {
                one: Some(9.0),
                five: Some(7.0),
                fifteen: Some(5.0),
                cpu_count: 4,
                recommendation,
            },
            memory: None,
            processes: ProcessSummary {
                relevant_count: 0,
                top_cpu: Vec::new(),
                top_rss: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            rig_leases: RigLeaseSummary {
                active_count: 0,
                leases: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            notes: Vec::new(),
        }
    }

    #[test]
    fn warns_when_hot_command_runs_on_warm_or_hot_machine() {
        let warning = evaluate(
            HotCommand { label: "bench" },
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");
        assert_eq!(warning.command, "bench");
        assert_eq!(warning.recommendation, ResourceRecommendation::Hot);
        assert!(warning.message.contains("--force-hot"));
        assert!(warning.message.contains("--runner <id>"));
        assert!(warning.message.contains("Load average is 9.0"));

        assert!(evaluate(
            HotCommand { label: "bench" },
            &resources(ResourceRecommendation::Warm)
        )
        .is_some());
    }

    #[test]
    fn does_not_warn_when_machine_is_ok() {
        assert!(evaluate(
            HotCommand { label: "bench" },
            &resources(ResourceRecommendation::Ok)
        )
        .is_none());
    }

    #[test]
    fn context_records_severity_warning_and_host_snapshot_when_hot() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(HotCommand { label: "bench" }, &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            HotCommand { label: "bench" },
            &resources,
            Some(&warning),
            false,
        );

        assert_eq!(context.command, "bench");
        assert_eq!(context.severity, "hot");
        assert!(!context.force_hot);
        assert!(context.warned);
        assert!(context
            .message
            .as_deref()
            .expect("message")
            .contains("Resource policy warning"));
        assert_eq!(context.host.load_severity, "hot");
        assert_eq!(context.host.load_one, Some(9.0));
        assert_eq!(context.host.cpu_count, 4);
        assert_eq!(context.host.memory_severity, None);
        assert_eq!(context.host.relevant_process_count, 0);
        assert_eq!(context.host.process_severity, "ok");
        assert_eq!(context.host.active_rig_lease_count, 0);
        assert_eq!(context.host.rig_lease_severity, "ok");
    }

    #[test]
    fn context_records_force_hot_bypass_for_hot_machine() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(HotCommand { label: "bench" }, &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            HotCommand { label: "bench" },
            &resources,
            Some(&warning),
            true,
        );

        assert!(context.force_hot);
        assert!(context.warned);
        assert_eq!(context.severity, "hot");
        assert!(context.message.is_some());
    }

    #[test]
    fn context_records_ok_machine_with_no_warning() {
        let resources = resources(ResourceRecommendation::Ok);
        assert!(evaluate(HotCommand { label: "bench" }, &resources).is_none());
        let context = ResourcePolicyContext::from_evaluation(
            HotCommand { label: "bench" },
            &resources,
            None,
            false,
        );

        assert_eq!(context.severity, "ok");
        assert!(!context.warned);
        assert!(context.message.is_none());
        assert!(!context.force_hot);
    }

    #[test]
    fn context_includes_memory_snapshot_when_available() {
        let mut resources = resources(ResourceRecommendation::Warm);
        resources.memory = Some(MemorySummary {
            total_mb: 32_000,
            available_mb: 1_500,
            used_percent: 95.3,
            recommendation: ResourceRecommendation::Warm,
        });
        let context = ResourcePolicyContext::from_evaluation(
            HotCommand { label: "bench" },
            &resources,
            None,
            false,
        );

        assert_eq!(context.host.memory_severity.as_deref(), Some("warm"));
        assert_eq!(context.host.memory_used_percent, Some(95.3));
        assert_eq!(context.host.memory_available_mb, Some(1_500));
    }

    #[test]
    fn context_serializes_to_json_with_expected_keys() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(HotCommand { label: "bench" }, &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            HotCommand { label: "bench" },
            &resources,
            Some(&warning),
            false,
        );
        let value = context.to_json();

        assert_eq!(value["command"], "bench");
        assert_eq!(value["severity"], "hot");
        assert_eq!(value["force_hot"], false);
        assert_eq!(value["warned"], true);
        assert!(value["message"].is_string());
        assert_eq!(value["host"]["load_severity"], "hot");
        assert_eq!(value["host"]["cpu_count"], 4);
    }
}
