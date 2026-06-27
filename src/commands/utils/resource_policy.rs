use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::cli_surface::Commands;
use crate::command_contract::LabCommandPortability;
use crate::commands::agent_task;

use crate::commands::resources::{DoctorOutput, ResourceRecommendation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotCommand {
    pub label: &'static str,
    pub lab_offload_supported: bool,
    pub lab_offload_unsupported_reason: Option<&'static str>,
}

impl HotCommand {
    pub fn lab_supported(label: &'static str) -> Self {
        Self {
            label,
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
        }
    }

    fn local_only(label: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            label,
            lab_offload_supported: false,
            lab_offload_unsupported_reason: reason,
        }
    }
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
    if is_read_only_agent_task(command) {
        return None;
    }

    let contract = command.lab_contract()?;

    match contract.portability {
        LabCommandPortability::Portable => Some(HotCommand::lab_supported(contract.hot_label)),
        LabCommandPortability::LocalOnly(reason) => {
            Some(HotCommand::local_only(contract.hot_label, Some(reason)))
        }
    }
}

fn is_read_only_agent_task(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Status(_)
                | agent_task::AgentTaskCommand::Logs(_)
                | agent_task::AgentTaskCommand::Artifacts(_)
                | agent_task::AgentTaskCommand::Review(_),
        })
    )
}

pub fn evaluate(command: HotCommand, resources: &DoctorOutput) -> Option<ResourcePolicyWarning> {
    evaluate_with_runner_hint(command, resources, None)
}

pub fn evaluate_with_runner_hint(
    command: HotCommand,
    resources: &DoctorOutput,
    default_runner: Option<&str>,
) -> Option<ResourcePolicyWarning> {
    match resources.recommendation {
        ResourceRecommendation::Ok => None,
        recommendation => Some(ResourcePolicyWarning {
            command: command.label,
            recommendation,
            message: warning_message(command, recommendation, resources, default_runner),
        }),
    }
}

pub fn non_interactive_preflight_error(
    warning: &ResourcePolicyWarning,
    force_hot: bool,
    interactive: bool,
    local_hot_rerun_command: Option<String>,
    default_runner: Option<&str>,
) -> Option<crate::core::Error> {
    if force_hot || interactive || is_runner_hosted_exec() {
        return None;
    }
    if default_runner.is_some() && warning.message.contains("--runner") {
        return None;
    }

    let mut error = crate::core::Error::validation_invalid_argument(
        "resource-policy",
        format!(
            "Refusing to start `{}` on a {} machine from a non-interactive shell. {} Use a safe Lab/offload path once this command supports it, or rerun later when `homeboy self doctor` reports ok.",
            warning.command,
            severity_str(warning.recommendation),
            primary_action(warning, default_runner),
        ),
        None,
        None,
    );
    if let Some(command) = local_hot_rerun_command {
        error.details["rerun_command"] = serde_json::Value::String(command);
    }
    Some(error)
}

pub fn local_hot_rerun_command(command: HotCommand, args: &[String]) -> Option<String> {
    if command.lab_offload_supported || args.is_empty() {
        return None;
    }

    let mut rerun = Vec::with_capacity(args.len() + 2);
    rerun.push(args[0].clone());
    if !args.iter().any(|arg| arg == "--force-hot") {
        rerun.push("--force-hot".to_string());
    }
    if !args.iter().any(|arg| arg == "--allow-local-hot") {
        rerun.push("--allow-local-hot".to_string());
    }
    rerun.extend(args.iter().skip(1).cloned());

    Some(crate::core::engine::shell::quote_args(&rerun))
}

pub fn is_runner_hosted_exec() -> bool {
    std::env::var(crate::core::runner::RUNNER_HOSTED_EXEC_ENV)
        .ok()
        .is_some_and(|value| value == "1")
}

pub fn clear_runner_hosted_exec() {
    std::env::remove_var(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
}

fn primary_action(warning: &ResourcePolicyWarning, default_runner: Option<&str>) -> String {
    if warning.message.contains("--runner <id>") {
        return "Connect a default Homeboy Lab runner or pass --runner <id> when Lab offload supports this mode.".to_string();
    }
    if let Some(runner_id) = default_runner {
        if warning.message.contains("--runner") {
            return format!(
                "Homeboy found Lab runner `{runner_id}`; rerun with --runner {runner_id} or let automatic Lab routing handle this portable command."
            );
        }
    }
    if warning.message.contains("--runner") {
        "Pass --runner <id> when Lab offload supports this mode.".to_string()
    } else {
        "This command is currently local-only under resource policy.".to_string()
    }
}

fn warning_message(
    command: HotCommand,
    recommendation: ResourceRecommendation,
    resources: &DoctorOutput,
    default_runner: Option<&str>,
) -> String {
    let severity = severity_str(recommendation);
    let reason = primary_reason(resources);
    if command.lab_offload_supported {
        if let Some(runner_id) = default_runner {
            return format!(
                "Resource policy warning: machine is {severity}; starting `{}` locally may skew results or add pressure. {reason} Homeboy found Lab runner `{runner_id}`; use --runner {runner_id} to route this portable command through Lab offload, or omit local-hot overrides so automatic Lab routing can use it.",
                command.label
            );
        }
        return format!(
            "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} Connect a default Homeboy Lab runner or use --runner <id> to route this portable command through Lab offload, or use --force-hot to run locally without this warning.",
            command.label
        );
    }

    let local_only_reason = command.lab_offload_unsupported_reason.unwrap_or(
        "This resource-pressure command does not have a portable Lab offload contract yet.",
    );
    format!(
        "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} {local_only_reason} Use --force-hot to run locally without this warning.",
        command.label
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

    "Run `homeboy self doctor` for details.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::Cli;
    use crate::commands::resources::{
        LoadSummary, MemorySummary, ProcessSummary, RigLeaseSummary,
    };
    use clap::Parser;
    fn resources(recommendation: ResourceRecommendation) -> DoctorOutput {
        DoctorOutput {
            command: "self.resources",
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

    fn lab_supported_hot(label: &'static str) -> HotCommand {
        HotCommand {
            label,
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
        }
    }

    fn local_only_hot(label: &'static str, reason: &'static str) -> HotCommand {
        HotCommand {
            label,
            lab_offload_supported: false,
            lab_offload_unsupported_reason: Some(reason),
        }
    }

    #[test]
    fn warns_when_hot_command_runs_on_warm_or_hot_machine() {
        let warning = evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");
        assert_eq!(warning.command, "bench");
        assert_eq!(warning.recommendation, ResourceRecommendation::Hot);
        assert!(warning.message.contains("--force-hot"));
        assert!(warning.message.contains("--runner <id>"));
        assert!(warning.message.contains("Load average is 9.0"));

        assert!(evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Warm)
        )
        .is_some());
    }

    #[test]
    fn warning_names_default_lab_runner_when_available() {
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task providers"),
            &resources(ResourceRecommendation::Hot),
            Some("homeboy-lab"),
        )
        .expect("hot machines warn");

        assert!(warning.message.contains("Lab runner `homeboy-lab`"));
        assert!(warning.message.contains("--runner homeboy-lab"));
        assert!(!warning.message.contains("--runner <id>"));
    }

    #[test]
    fn warning_for_local_only_hot_command_explains_runner_unavailability() {
        let warning = evaluate(
            local_only_hot("rig up", "`rig up` stays local for test reasons."),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(warning.message.contains("`rig up` stays local"));
        assert!(warning.message.contains("--force-hot"));
        assert!(!warning.message.contains("--runner <id>"));
    }

    #[test]
    fn changed_scope_commands_are_hot_but_file_scoped_lint_is_not() {
        let changed_lint = Cli::parse_from(["homeboy", "lint", "--changed-since", "origin/main"]);
        let hot = hot_command(&changed_lint.command).expect("changed-scope lint is hot");
        assert_eq!(hot.label, "lint");
        assert!(hot.lab_offload_supported);
        assert!(hot.lab_offload_unsupported_reason.is_none());

        let file_lint = Cli::parse_from(["homeboy", "lint", "--file", "src/main.rs"]);
        assert!(hot_command(&file_lint.command).is_none());
    }

    #[test]
    fn agent_task_inspection_commands_do_not_start_hot_workloads() {
        for args in [
            ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            assert!(hot_command(&cli.command).is_none());
        }
    }

    #[test]
    fn does_not_warn_when_machine_is_ok() {
        assert!(evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Ok)
        )
        .is_none());
    }

    #[test]
    fn non_interactive_hot_warning_fails_before_starting_command() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate(
            lab_supported_hot("audit"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        let error = non_interactive_preflight_error(&warning, false, false, None, None)
            .expect("non-interactive hot runs should fail fast");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("Refusing to start `audit`"));
        assert!(error.message.contains("non-interactive shell"));
        assert!(error.message.contains("--runner <id>"));
        assert!(!error.message.contains("Use --force-hot"));
        assert!(error.details.get("rerun_command").is_none());
    }

    #[test]
    fn non_interactive_hot_warning_allows_default_lab_runner_auto_offload() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task providers"),
            &resources(ResourceRecommendation::Hot),
            Some("homeboy-lab"),
        )
        .expect("hot machines warn");

        assert!(
            non_interactive_preflight_error(&warning, false, false, None, Some("homeboy-lab"))
                .is_none()
        );
    }

    #[test]
    fn non_interactive_local_only_refusal_includes_local_hot_rerun_command() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let command = local_only_hot(
            "lint",
            "Changed-scope lint runs stay local because changed-file scopes are not represented in the current Lab portability contract yet.",
        );
        let warning = evaluate(command, &resources(ResourceRecommendation::Warm))
            .expect("warm machines warn");
        let rerun = local_hot_rerun_command(
            command,
            &[
                "homeboy".to_string(),
                "lint".to_string(),
                "--changed-since".to_string(),
                "origin/main".to_string(),
            ],
        );

        let error = non_interactive_preflight_error(&warning, false, false, rerun, None)
            .expect("non-interactive local-only hot runs should fail fast");

        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --force-hot --allow-local-hot lint --changed-since origin/main")
        );
        assert!(!error.message.contains("--force-hot --allow-local-hot"));
    }

    #[test]
    fn runner_hosted_exec_does_not_fail_non_interactive_preflight() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::set(crate::core::runner::RUNNER_HOSTED_EXEC_ENV, "1");
        let warning = evaluate(
            lab_supported_hot("agent-task cook/run-plan"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, None).is_none());
    }

    #[test]
    fn interactive_or_forced_hot_warning_does_not_fail_preflight() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate(
            lab_supported_hot("audit"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, true, None, None).is_none());
        assert!(non_interactive_preflight_error(&warning, true, false, None, None).is_none());
    }

    #[test]
    fn context_records_severity_warning_and_host_snapshot_when_hot() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            lab_supported_hot("bench"),
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
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            lab_supported_hot("bench"),
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
        assert!(evaluate(lab_supported_hot("bench"), &resources).is_none());
        let context = ResourcePolicyContext::from_evaluation(
            lab_supported_hot("bench"),
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
            lab_supported_hot("bench"),
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
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            lab_supported_hot("bench"),
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

    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, prior }
        }

        fn remove(name: &'static str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, prior }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("resource policy env test lock poisoned")
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}
