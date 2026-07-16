use crate::cli_surface::Commands;
use crate::command_contract::LabCommandPortability;
use crate::commands::agent_task;

use crate::commands::resources::{DoctorOutput, ResourceRecommendation};

// The captured resource-policy context type, its process-wide store, and the
// runner-placement environment probes moved to `core::resource_policy_context`
// so `core::runner` can read them without a core -> commands dependency edge.
// Re-exported here to keep existing `resource_policy::*` call sites working.
#[cfg(test)]
pub use crate::core::resource_policy_context::reset_captured_context_for_test;
pub use crate::core::resource_policy_context::{
    capture_context, captured_context, clear_managed_runner_placement_context,
    clear_runner_hosted_exec, is_ci_execution, is_managed_runner_placement_context,
    is_runner_hosted_exec, ResourcePolicyContext, ResourcePolicyHostSnapshot,
    ResourcePolicyRunnerSelection,
};

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

/// Build a structured `ResourcePolicyContext` from a `DoctorOutput`, the matched
/// hot command, an optional warning, and whether local placement was explicit.
///
/// A free function (rather than an inherent method) because `ResourcePolicyContext`
/// is defined in the `homeboy-core` crate and this construction depends on
/// CLI-layer types.
pub fn resource_policy_context_from_evaluation(
    command: HotCommand,
    resources: &DoctorOutput,
    warning: Option<&ResourcePolicyWarning>,
    local_override: bool,
    default_runner: Option<&str>,
    runner_hosted: bool,
) -> ResourcePolicyContext {
    let runner_selection =
        runner_selection_context(command, local_override, default_runner, runner_hosted);
    ResourcePolicyContext {
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

/// Serialize a `ResourcePolicyContext` as the JSON value that lands inside
/// observation `metadata_json["resource_policy"]`.
pub fn resource_policy_context_to_json(context: &ResourcePolicyContext) -> serde_json::Value {
    serde_json::to_value(context).unwrap_or(serde_json::Value::Null)
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
) -> Option<crate::core::Error> {
    // GitHub Actions runners are ephemeral, single-purpose, and always
    // non-interactive: the warm-machine refusal would fail otherwise-good PR
    // checks with no human to rerun and no Lab runner to route to. Never refuse
    // inside CI (#7735).
    if local_override || interactive || is_runner_hosted_exec() || is_ci_execution() {
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

fn primary_action(warning: &ResourcePolicyWarning, default_runner: Option<&str>) -> String {
    // A portable command's warning names either a concrete default runner
    // ("Lab runner `<id>`") or, when none is configured, states Lab offload is
    // unavailable. Local-only commands say neither. Branch on those facts rather
    // than inferring Lab availability from placeholder substrings.
    if let Some(runner_id) = default_runner {
        if warning.message.contains(&format!("`{runner_id}`")) {
            return format!(
                "Homeboy found Lab runner `{runner_id}`; rerun with --runner {runner_id} or let automatic Lab routing handle this portable command."
            );
        }
    }
    if warning
        .message
        .contains("Lab offload is not currently available")
    {
        // Portable command, but no Lab runner is configured on this host (#7749):
        // do not point the operator at nonexistent Lab infrastructure.
        return "No Homeboy Lab runner is configured on this host, so Lab offload is not available. Defer verification to CI, connect a runner with `homeboy runner connect`, or use the local-hot rerun command only if local execution is explicitly authorized.".to_string();
    }
    if warning.message.contains("--runner") {
        "Pass --runner <id> when Lab offload supports this mode.".to_string()
    } else {
        "Lab routing is not offered because this command is currently local-only under resource policy.".to_string()
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
            "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} No Homeboy Lab runner is configured on this host, so Lab offload is not currently available; connect a runner (`homeboy runner connect`) to enable it, defer verification to CI, or use --placement local to run locally without this warning.",
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
        assert!(warning
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
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
    fn verified_agent_task_cook_keeps_its_controller_on_a_warm_controller() {
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
        assert!(!command.lab_offload_supported);
        assert_eq!(command.label, "agent-task cook/run-plan/retry --run");
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
        assert!(error
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
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

        let error = non_interactive_preflight_error(&warning, false, false, rerun, None)
            .expect("non-interactive local-only hot runs should fail fast");

        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --placement local review lint --changed-since origin/main")
        );
        assert!(error.message.contains("Lab routing is not offered"));
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
        let error = non_interactive_preflight_error(&warning, false, false, rerun, None)
            .expect("non-interactive hot runs should fail fast");

        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --placement local audit")
        );
        assert!(error
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
        // #7749: the refusal must not point the operator at Lab infrastructure
        // that does not exist on this host.
        assert!(!error
            .message
            .contains("Connect a default Homeboy Lab runner"));
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
        let context = resource_policy_context_from_evaluation(
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
        let context = resource_policy_context_from_evaluation(
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
        let context = resource_policy_context_from_evaluation(
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
        let context = resource_policy_context_from_evaluation(
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
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            Some(&warning),
            false,
            Some("homeboy-lab"),
            false,
        );
        let value = resource_policy_context_to_json(&context);

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

    #[test]
    fn ci_execution_does_not_fail_non_interactive_preflight() {
        // #7735: inside GitHub Actions the warm-machine refusal must not fire.
        // The runner is ephemeral and non-interactive by design; refusing there
        // fails otherwise-good PR checks.
        let _lock = env_lock();
        let _hosted = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::set("GITHUB_ACTIONS", "true");
        let warning = evaluate(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, None).is_none());
    }

    #[test]
    fn non_ci_shell_still_refuses_when_warm() {
        // Guard against the CI bypass leaking into ordinary non-interactive
        // shells (e.g. cron, agent runners) where the refusal is still correct.
        let _lock = env_lock();
        let _hosted = EnvVarGuard::remove(crate::core::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let warning = evaluate(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, None).is_some());
    }

    #[test]
    fn portable_warning_without_runner_does_not_advertise_lab() {
        // #7749: on a host with no Lab runner configured, the warning must not
        // recommend connecting/using a Lab runner as if one were available.
        let warning = evaluate(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(warning
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
        assert!(warning
            .message
            .contains("Lab offload is not currently available"));
        assert!(!warning
            .message
            .contains("Connect a default Homeboy Lab runner"));
        // A genuinely configured runner should still be named.
        let with_runner = evaluate_with_runner_hint(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
            Some("homeboy-lab"),
        )
        .expect("hot machines warn");
        assert!(with_runner.message.contains("Lab runner `homeboy-lab`"));
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
