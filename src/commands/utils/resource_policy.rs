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
    /// Requested controller placement. This records an intentional local
    /// override without preserving deprecated CLI terminology.
    pub local_override: bool,
    /// Whether a warning was emitted (or would have been) for this run.
    pub warned: bool,
    /// Human-readable warning message produced by the policy, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Controller-side routing decision available at resource-policy preflight.
    pub runner_selection: ResourcePolicyRunnerSelection,
    /// Structured host snapshot used to derive the severity.
    pub host: ResourcePolicyHostSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourcePolicyRunnerSelection {
    /// Runner selected before command execution, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    /// Why this command is expected to run locally or through Lab.
    pub reason: String,
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
    /// command, an optional warning, and whether local placement was explicit.
    pub fn from_evaluation(
        command: HotCommand,
        resources: &DoctorOutput,
        warning: Option<&ResourcePolicyWarning>,
        local_override: bool,
        default_runner: Option<&str>,
        runner_hosted: bool,
    ) -> Self {
        let runner_selection =
            runner_selection_context(command, local_override, default_runner, runner_hosted);
        Self {
            command: command.label.to_string(),
            severity: severity_str(resources.recommendation).to_string(),
            local_override,
            warned: warning.is_some(),
            message: warning.map(|warning| warning.message.clone()),
            runner_selection,
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

fn runner_selection_context(
    command: HotCommand,
    local_override: bool,
    default_runner: Option<&str>,
    runner_hosted: bool,
) -> ResourcePolicyRunnerSelection {
    if runner_hosted {
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            reason: "runner_hosted".to_string(),
        };
    }
    if local_override {
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            reason: "placement_local_override".to_string(),
        };
    }
    if command.lab_offload_supported {
        if let Some(runner_id) = default_runner {
            return ResourcePolicyRunnerSelection {
                runner_id: Some(runner_id.to_string()),
                reason: "default_lab_runner".to_string(),
            };
        }
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            reason: "local_no_default_runner".to_string(),
        };
    }
    ResourcePolicyRunnerSelection {
        runner_id: None,
        reason: "local_only_contract".to_string(),
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
    if is_plan_only_command(command) || is_read_only_agent_task(command) {
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

fn is_plan_only_command(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                command: agent_task::AgentTaskFanoutCommand::CookBatch(
                    agent_task::AgentTaskFanoutCookBatchArgs { dry_run: true, .. },
                ),
            }),
        })
    )
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
    local_override: bool,
    interactive: bool,
    local_hot_rerun_command: Option<String>,
    default_runner: Option<&str>,
    command: HotCommand,
) -> Option<crate::core::Error> {
    if local_override || interactive || is_runner_hosted_exec() {
        return None;
    }
    if default_runner.is_some() && warning.message.contains("--runner") {
        return None;
    }

    let mut error = crate::core::Error::validation_invalid_argument(
        "resource-policy",
        format!(
            "Refusing to start `{}` on a {} machine from a non-interactive shell. {} Rerun later when `homeboy self doctor` reports ok.",
            warning.command,
            severity_str(warning.recommendation),
            primary_action(warning, default_runner),
        ),
        None,
        None,
    );

    if command.lab_offload_supported {
        // Portable command: the safe rerun (Lab `--runner`, or explicit local
        // placement when the operator wants it) is the primary path.
        if let Some(rerun) = local_hot_rerun_command {
            error.details["rerun_command"] = serde_json::Value::String(rerun);
        }
    } else {
        // Non-portable command: do NOT surface a local `--placement local`
        // rerun as the primary path — for a non-interactive agent that turns a
        // safety refusal into a copy-paste local bypass (#6384). Instead record
        // the Lab-portability gap and file/fix guidance, and demote the local
        // rerun to a clearly operator-only override that agents must not take by
        // default.
        error.details["lab_portability_gap"] = serde_json::Value::String(format!(
            "`{}` has no portable Lab offload contract yet: {} A non-interactive agent should stop here and open/track a Lab-portability issue for this command rather than run it locally.",
            warning.command,
            command.lab_offload_unsupported_reason.unwrap_or(
                "this resource-pressure command is currently local-only under resource policy.",
            ),
        ));
        if let Some(rerun) = local_hot_rerun_command {
            error.details["operator_only_local_override"] = serde_json::Value::String(rerun);
        }
    }

    Some(error)
}

pub fn rerun_command(
    command: HotCommand,
    args: &[String],
    default_runner: Option<&str>,
) -> Option<String> {
    if args.is_empty() {
        return None;
    }

    let mut rerun = Vec::with_capacity(args.len() + 2);
    rerun.push(args[0].clone());
    if command.lab_offload_supported {
        if let Some(runner_id) = default_runner {
            if !args.iter().any(|arg| arg == "--runner") {
                rerun.push("--runner".to_string());
                rerun.push(runner_id.to_string());
            }
        } else {
            append_local_placement(&mut rerun, args);
        }
    } else {
        append_local_placement(&mut rerun, args);
    }
    rerun.extend(args.iter().skip(1).cloned());

    Some(crate::core::engine::shell::quote_args(&rerun))
}

fn append_local_placement(rerun: &mut Vec<String>, args: &[String]) {
    if !args
        .iter()
        .any(|arg| arg == "--placement" || arg.starts_with("--placement="))
    {
        rerun.push("--placement".to_string());
        rerun.push("local".to_string());
    }
}

pub fn is_runner_hosted_exec() -> bool {
    std::env::var(crate::core::runner::RUNNER_HOSTED_EXEC_ENV)
        .ok()
        .is_some_and(|value| value == "1")
}

/// True for the complete environment prepared by a managed remote runner exec.
///
/// Environment variables are not cryptographic provenance: a process with
/// arbitrary environment control can forge them. They are an internal runner
/// transport boundary, so require all three values emitted by RunnerExecOptions
/// and daemon job preparation rather than trusting the resolved marker alone.
pub fn is_managed_runner_placement_context() -> bool {
    is_runner_hosted_exec()
        && std::env::var(crate::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV)
            .ok()
            .is_some_and(|value| value == "1")
        && std::env::var(crate::core::runner::RUNNER_ID_ENV)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
}

pub fn clear_runner_hosted_exec() {
    std::env::remove_var(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
}

/// Remove the private runner-placement transport context after CLI routing.
/// Child workloads and persisted artifacts must not inherit controller routing
/// markers once their single placement decision has been consumed.
pub fn clear_managed_runner_placement_context() {
    std::env::remove_var(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
    std::env::remove_var(crate::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV);
    std::env::remove_var(crate::core::runner::RUNNER_ID_ENV);
}

fn primary_action(warning: &ResourcePolicyWarning, default_runner: Option<&str>) -> String {
    if warning.message.contains("--runner <id>") {
        return "No eligible Homeboy Lab runner was found. Connect a runner or pass --runner <id> to offload this portable command; use the local-hot rerun command only as a last resort.".to_string();
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
        "Lab routing is not offered because this command has no portable Lab offload contract yet; it needs Lab-portability work (see the lab_portability_gap detail). The local override is operator-only and is not a safe automated rerun.".to_string()
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
            "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} Connect a default Homeboy Lab runner or use --runner <id> to route this portable command through Lab offload, or use --placement local to run locally without this warning.",
            command.label
        );
    }

    let local_only_reason = command.lab_offload_unsupported_reason.unwrap_or(
        "This resource-pressure command does not have a portable Lab offload contract yet.",
    );
    format!(
        "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} {local_only_reason} Use --placement local to run locally without this warning.",
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
    use crate::commands::resources::{LoadSummary, MemorySummary, ProcessSummary, RigLeaseSummary};
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
        assert!(warning.message.contains("--placement local"));
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
        assert!(warning.message.contains("--placement local"));
        assert!(!warning.message.contains("--runner <id>"));
    }

    #[test]
    fn changed_and_file_scoped_lint_commands_are_hot() {
        let changed_lint = Cli::parse_from([
            "homeboy",
            "review",
            "lint",
            "--changed-since",
            "origin/main",
        ]);
        let hot = hot_command(&changed_lint.command).expect("changed-scope lint is hot");
        assert_eq!(hot.label, "review lint");
        assert!(hot.lab_offload_supported);
        assert!(hot.lab_offload_unsupported_reason.is_none());

        let file_lint = Cli::parse_from(["homeboy", "review", "lint", "--file", "src/main.rs"]);
        let hot = hot_command(&file_lint.command).expect("file-scope lint is hot");
        assert_eq!(hot.label, "review lint");
        assert!(hot.lab_offload_supported);
        assert!(hot.lab_offload_unsupported_reason.is_none());
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
    fn agent_task_cook_batch_dry_run_does_not_start_hot_workloads() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--verify",
            "cargo build -j 3",
            "--dry-run",
            "https://github.com/Extra-Chill/homeboy/issues/7796",
        ]);

        assert!(hot_command(&cli.command).is_none());
    }

    #[test]
    fn verified_agent_task_cook_automatically_selects_default_lab_on_warm_controller() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
            "--verify",
            "cargo test --locked",
        ]);
        let command = hot_command(&cli.command).expect("verified cook is hot");
        let resources = resources(ResourceRecommendation::Warm);
        let warning = evaluate_with_runner_hint(command, &resources, Some("homeboy-lab"))
            .expect("warm controller warns before routing");
        let context = ResourcePolicyContext::from_evaluation(
            command,
            &resources,
            Some(&warning),
            false,
            Some("homeboy-lab"),
            false,
        );

        assert!(command.lab_offload_supported);
        assert_eq!(command.label, "agent-task cook/run-plan/retry --run");
        assert_eq!(
            context.runner_selection,
            ResourcePolicyRunnerSelection {
                runner_id: Some("homeboy-lab".to_string()),
                reason: "default_lab_runner".to_string(),
            }
        );
        assert!(non_interactive_preflight_error(
            &warning,
            false,
            false,
            None,
            Some("homeboy-lab"),
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
        )
        .is_none());
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

        let error = non_interactive_preflight_error(
            &warning,
            false,
            false,
            None,
            None,
            lab_supported_hot("audit"),
        )
        .expect("non-interactive hot runs should fail fast");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("Refusing to start `audit`"));
        assert!(error.message.contains("non-interactive shell"));
        assert!(error.message.contains("--runner <id>"));
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

        assert!(non_interactive_preflight_error(
            &warning,
            false,
            false,
            None,
            Some("homeboy-lab"),
            lab_supported_hot("agent-task providers"),
        )
        .is_none());
    }

    #[test]
    fn non_interactive_local_only_refusal_demotes_local_override_and_flags_portability_gap() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let command = local_only_hot(
            "lint",
            "Changed-scope lint runs stay local because changed-file scopes are not represented in the current Lab portability contract yet.",
        );
        let warning = evaluate(command, &resources(ResourceRecommendation::Warm))
            .expect("warm machines warn");
        let rerun = rerun_command(
            command,
            &[
                "homeboy".to_string(),
                "review".to_string(),
                "lint".to_string(),
                "--changed-since".to_string(),
                "origin/main".to_string(),
            ],
            None,
        );

        let error = non_interactive_preflight_error(&warning, false, false, rerun, None, command)
            .expect("non-interactive local-only hot runs should fail fast");

        // The local `--placement local` command must NOT be the primary rerun
        // path handed to a non-interactive agent (#6384). It is demoted to an
        // explicitly operator-only field, and the refusal records the
        // Lab-portability gap the agent should stop and track instead.
        assert!(
            error.details.get("rerun_command").is_none(),
            "non-portable refusal must not offer a local rerun as the primary path"
        );
        assert_eq!(
            error.details["operator_only_local_override"].as_str(),
            Some("homeboy --placement local review lint --changed-since origin/main")
        );
        let gap = error.details["lab_portability_gap"]
            .as_str()
            .expect("portability gap recorded");
        assert!(gap.contains("no portable Lab offload contract yet"));
        assert!(gap.contains("open/track a Lab-portability issue"));
        assert!(error.message.contains("needs Lab-portability work"));
        assert!(error.message.contains("operator-only"));
    }

    #[test]
    fn portable_refusal_rerun_uses_eligible_lab_runner() {
        let rerun = rerun_command(
            lab_supported_hot("audit"),
            &[
                "homeboy".to_string(),
                "audit".to_string(),
                "--changed-since".to_string(),
                "origin/main".to_string(),
            ],
            Some("homeboy-lab"),
        );

        assert_eq!(
            rerun.as_deref(),
            Some("homeboy --runner homeboy-lab audit --changed-since origin/main")
        );
    }

    #[test]
    fn portable_refusal_without_runner_uses_explicit_local_last_resort() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate(
            lab_supported_hot("audit"),
            &resources(ResourceRecommendation::Warm),
        )
        .expect("warm machines warn");
        let rerun = rerun_command(
            lab_supported_hot("audit"),
            &["homeboy".to_string(), "audit".to_string()],
            None,
        );
        let error = non_interactive_preflight_error(
            &warning,
            false,
            false,
            rerun,
            None,
            lab_supported_hot("audit"),
        )
        .expect("non-interactive hot runs should fail fast");

        // Portable command with no connected runner: the explicit local rerun
        // stays the (last-resort) primary path — it is Lab-portable, the runner
        // is simply not connected. No portability-gap footgun here.
        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --placement local audit")
        );
        assert!(error.details.get("lab_portability_gap").is_none());
        assert!(error
            .message
            .contains("No eligible Homeboy Lab runner was found"));
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

        assert!(non_interactive_preflight_error(
            &warning,
            false,
            false,
            None,
            None,
            lab_supported_hot("agent-task cook/run-plan"),
        )
        .is_none());
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

        assert!(non_interactive_preflight_error(
            &warning,
            false,
            true,
            None,
            None,
            lab_supported_hot("audit"),
        )
        .is_none());
        assert!(non_interactive_preflight_error(
            &warning,
            true,
            false,
            None,
            None,
            lab_supported_hot("audit"),
        )
        .is_none());
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
            Some("homeboy-lab"),
            false,
        );

        assert_eq!(context.command, "bench");
        assert_eq!(context.severity, "hot");
        assert!(!context.local_override);
        assert!(context.warned);
        assert!(context
            .message
            .as_deref()
            .expect("message")
            .contains("Resource policy warning"));
        assert_eq!(
            context.runner_selection,
            ResourcePolicyRunnerSelection {
                runner_id: Some("homeboy-lab".to_string()),
                reason: "default_lab_runner".to_string(),
            }
        );
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
    fn context_records_local_placement_for_hot_machine() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = ResourcePolicyContext::from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            Some(&warning),
            true,
            Some("homeboy-lab"),
            false,
        );

        assert!(context.local_override);
        assert!(context.warned);
        assert_eq!(context.severity, "hot");
        assert!(context.message.is_some());
        assert_eq!(context.runner_selection.reason, "placement_local_override");
        assert_eq!(context.runner_selection.runner_id, None);
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
            None,
            false,
        );

        assert_eq!(context.severity, "ok");
        assert!(!context.warned);
        assert!(context.message.is_none());
        assert!(!context.local_override);
        assert_eq!(context.runner_selection.reason, "local_no_default_runner");
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
            Some("homeboy-lab"),
            false,
        );
        let value = context.to_json();

        assert_eq!(value["command"], "bench");
        assert_eq!(value["severity"], "hot");
        assert_eq!(value["local_override"], false);
        assert_eq!(value["warned"], true);
        assert!(value["message"].is_string());
        assert_eq!(value["runner_selection"]["runner_id"], "homeboy-lab");
        assert_eq!(value["runner_selection"]["reason"], "default_lab_runner");
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
