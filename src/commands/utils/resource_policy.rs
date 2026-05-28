use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::cli_surface::{Commands, LabCommandPortability, LabCommandRequiredTool};

use crate::commands::doctor::resources::{DoctorOutput, ResourceRecommendation};
use crate::commands::runner::doctor::RunnerDoctorOutput;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerGateMode {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerTool {
    Git,
    Node,
    Npm,
    Pnpm,
    Php,
    Composer,
    Docker,
    Playwright,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerCapabilityPlan {
    pub command: &'static str,
    pub required_tools: Vec<LabRunnerTool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRunnerGateDecision {
    Eligible,
    Missing {
        runner_id: String,
        command: &'static str,
        missing_tools: Vec<LabRunnerTool>,
        reason: String,
        remediation: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabRunnerCapabilities {
    pub git: bool,
    pub node: bool,
    pub npm: bool,
    pub pnpm: bool,
    pub php: bool,
    pub composer: bool,
    pub docker: bool,
    pub playwright: bool,
}

impl LabRunnerCapabilities {
    pub fn from_doctor(report: &RunnerDoctorOutput) -> Self {
        Self {
            git: report.capabilities.git,
            node: report.capabilities.node,
            npm: report.capabilities.npm,
            pnpm: report.capabilities.pnpm,
            php: report.capabilities.php,
            composer: report.capabilities.composer,
            docker: report.capabilities.docker,
            playwright: report.capabilities.playwright && report.capabilities.browser_ready,
        }
    }

    fn has(&self, tool: LabRunnerTool) -> bool {
        match tool {
            LabRunnerTool::Git => self.git,
            LabRunnerTool::Node => self.node,
            LabRunnerTool::Npm => self.npm,
            LabRunnerTool::Pnpm => self.pnpm,
            LabRunnerTool::Php => self.php,
            LabRunnerTool::Composer => self.composer,
            LabRunnerTool::Docker => self.docker,
            LabRunnerTool::Playwright => self.playwright,
        }
    }
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
    let contract = command.lab_contract()?;

    match contract.portability {
        LabCommandPortability::Portable => Some(HotCommand::lab_supported(contract.hot_label)),
        LabCommandPortability::LocalOnly(reason) => {
            Some(HotCommand::local_only(contract.hot_label, Some(reason)))
        }
    }
}

pub fn lab_runner_capability_plan(
    command: &Commands,
    source_path: &Path,
) -> Option<LabRunnerCapabilityPlan> {
    let contract = command.lab_contract()?;
    let hot_command = hot_command(command)?;
    let mut required_tools = vec![LabRunnerTool::Git];

    if source_path.join("package.json").is_file() {
        push_unique(&mut required_tools, LabRunnerTool::Node);
        push_unique(&mut required_tools, LabRunnerTool::Npm);
    }

    if source_path.join("pnpm-lock.yaml").is_file() {
        push_unique(&mut required_tools, LabRunnerTool::Node);
        push_unique(&mut required_tools, LabRunnerTool::Pnpm);
    }

    if source_path.join("composer.json").is_file() {
        push_unique(&mut required_tools, LabRunnerTool::Php);
        push_unique(&mut required_tools, LabRunnerTool::Composer);
    }

    if has_docker_signal(source_path) {
        push_unique(&mut required_tools, LabRunnerTool::Docker);
    }

    for tool in contract.extra_required_tools {
        push_unique(&mut required_tools, lab_runner_tool(*tool));
    }

    Some(LabRunnerCapabilityPlan {
        command: hot_command.label,
        required_tools,
    })
}

fn lab_runner_tool(tool: LabCommandRequiredTool) -> LabRunnerTool {
    match tool {
        LabCommandRequiredTool::Playwright => LabRunnerTool::Playwright,
    }
}

pub fn evaluate_lab_runner_capabilities(
    runner_id: &str,
    plan: &LabRunnerCapabilityPlan,
    capabilities: &LabRunnerCapabilities,
    mode: LabRunnerGateMode,
) -> LabRunnerGateDecision {
    let missing_tools = plan
        .required_tools
        .iter()
        .copied()
        .filter(|tool| !capabilities.has(*tool))
        .collect::<Vec<_>>();

    if missing_tools.is_empty() {
        return LabRunnerGateDecision::Eligible;
    }

    let missing = tool_list(&missing_tools);
    let reason = match mode {
        LabRunnerGateMode::Automatic => format!(
            "Local fallback: runner '{runner_id}' is missing required tool(s) for `{}`: {missing}.",
            plan.command
        ),
        LabRunnerGateMode::Explicit => format!(
            "Lab offload runner '{runner_id}' is missing required tool(s) for `{}`: {missing}.",
            plan.command
        ),
    };
    let mut remediation = missing_tools
        .iter()
        .map(|tool| tool.remediation().to_string())
        .collect::<Vec<_>>();
    match mode {
        LabRunnerGateMode::Automatic => remediation.push(
            "Homeboy will run locally instead of auto-offloading to this runner.".to_string(),
        ),
        LabRunnerGateMode::Explicit => remediation.push(
            "Install the missing tool(s) on the runner, choose a capable runner, or omit --runner to run locally.".to_string(),
        ),
    }

    LabRunnerGateDecision::Missing {
        runner_id: runner_id.to_string(),
        command: plan.command,
        missing_tools,
        reason,
        remediation,
    }
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn has_docker_signal(source_path: &Path) -> bool {
    [
        "Dockerfile",
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ]
    .iter()
    .any(|name| source_path.join(name).is_file())
}

fn tool_list(tools: &[LabRunnerTool]) -> String {
    tools
        .iter()
        .map(|tool| tool.id())
        .collect::<Vec<_>>()
        .join(", ")
}

impl LabRunnerTool {
    pub fn id(self) -> &'static str {
        match self {
            LabRunnerTool::Git => "git",
            LabRunnerTool::Node => "node",
            LabRunnerTool::Npm => "npm",
            LabRunnerTool::Pnpm => "pnpm",
            LabRunnerTool::Php => "php",
            LabRunnerTool::Composer => "composer",
            LabRunnerTool::Docker => "docker",
            LabRunnerTool::Playwright => "playwright+browsers",
        }
    }

    fn remediation(self) -> &'static str {
        match self {
            LabRunnerTool::Git => "Install git and ensure it is on the runner PATH.",
            LabRunnerTool::Node => "Install Node.js and ensure node is on the runner PATH.",
            LabRunnerTool::Npm => "Install npm with Node.js and ensure npm is on the runner PATH.",
            LabRunnerTool::Pnpm => "Install pnpm for repositories with pnpm-lock.yaml.",
            LabRunnerTool::Php => "Install PHP for repositories with composer.json.",
            LabRunnerTool::Composer => "Install Composer for repositories with composer.json.",
            LabRunnerTool::Docker => "Install and start Docker for container-backed repositories.",
            LabRunnerTool::Playwright => {
                "Install Playwright CLI and browser binaries on the runner."
            }
        }
    }
}

pub fn evaluate(command: HotCommand, resources: &DoctorOutput) -> Option<ResourcePolicyWarning> {
    match resources.recommendation {
        ResourceRecommendation::Ok => None,
        recommendation => Some(ResourcePolicyWarning {
            command: command.label,
            recommendation,
            message: warning_message(command, recommendation, resources),
        }),
    }
}

fn warning_message(
    command: HotCommand,
    recommendation: ResourceRecommendation,
    resources: &DoctorOutput,
) -> String {
    let severity = severity_str(recommendation);
    let reason = primary_reason(resources);
    if command.lab_offload_supported {
        return format!(
            "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} Connect a default Homeboy Lab runner or use --runner <id> to offload this hot command, or use --force-hot to run locally without this warning.",
            command.label
        );
    }

    let local_only_reason = command
        .lab_offload_unsupported_reason
        .unwrap_or("This hot command does not have a portable Lab offload contract yet.");
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

    "Run `homeboy doctor resources` for details.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::resources::{
        LoadSummary, MemorySummary, ProcessSummary, RigLeaseSummary,
    };
    use crate::commands::lint::LintArgs;
    use crate::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
    };

    fn lint_command() -> Commands {
        Commands::Lint(LintArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            summary: false,
            file: None,
            glob: None,
            changed_only: false,
            changed_since: None,
            ci_job: None,
            errors_only: false,
            sniffs: None,
            exclude_sniffs: None,
            category: None,
            fix: false,
            force: false,
            setting_args: SettingArgs::default(),
            baseline_args: BaselineArgs::default(),
            json_summary: false,
        })
    }

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
    fn does_not_warn_when_machine_is_ok() {
        assert!(evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Ok)
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

    #[test]
    fn lab_runner_capability_plan_detects_project_tool_needs() {
        let dir = tempfile::tempdir().expect("temp project");
        std::fs::write(dir.path().join("package.json"), "{}").expect("package.json");
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: '9'\n")
            .expect("pnpm-lock");
        std::fs::write(dir.path().join("composer.json"), "{}").expect("composer.json");
        std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");

        let plan = lab_runner_capability_plan(&lint_command(), dir.path()).expect("plan");

        assert_eq!(plan.command, "lint");
        assert_eq!(
            plan.required_tools,
            vec![
                LabRunnerTool::Git,
                LabRunnerTool::Node,
                LabRunnerTool::Npm,
                LabRunnerTool::Pnpm,
                LabRunnerTool::Php,
                LabRunnerTool::Composer,
                LabRunnerTool::Docker,
            ]
        );
    }

    #[test]
    fn lab_runner_gate_allows_capable_runner() {
        let plan = LabRunnerCapabilityPlan {
            command: "lint",
            required_tools: vec![LabRunnerTool::Git, LabRunnerTool::Node, LabRunnerTool::Pnpm],
        };
        let capabilities = LabRunnerCapabilities {
            git: true,
            node: true,
            pnpm: true,
            ..LabRunnerCapabilities::default()
        };

        assert_eq!(
            evaluate_lab_runner_capabilities(
                "lab",
                &plan,
                &capabilities,
                LabRunnerGateMode::Explicit,
            ),
            LabRunnerGateDecision::Eligible
        );
    }

    #[test]
    fn lab_runner_gate_reports_missing_tool_for_explicit_runner() {
        let plan = LabRunnerCapabilityPlan {
            command: "test",
            required_tools: vec![LabRunnerTool::Git, LabRunnerTool::Pnpm],
        };
        let capabilities = LabRunnerCapabilities {
            git: true,
            ..LabRunnerCapabilities::default()
        };

        let decision = evaluate_lab_runner_capabilities(
            "lab",
            &plan,
            &capabilities,
            LabRunnerGateMode::Explicit,
        );

        let LabRunnerGateDecision::Missing {
            missing_tools,
            reason,
            remediation,
            ..
        } = decision
        else {
            panic!("expected missing tool decision");
        };
        assert_eq!(missing_tools, vec![LabRunnerTool::Pnpm]);
        assert!(reason.contains("Lab offload runner 'lab'"));
        assert!(reason.contains("pnpm"));
        assert!(remediation
            .iter()
            .any(|item| item.contains("omit --runner")));
    }

    #[test]
    fn lab_runner_gate_reports_local_fallback_for_auto_runner() {
        let plan = LabRunnerCapabilityPlan {
            command: "trace",
            required_tools: vec![LabRunnerTool::Git, LabRunnerTool::Playwright],
        };
        let capabilities = LabRunnerCapabilities {
            git: true,
            ..LabRunnerCapabilities::default()
        };

        let decision = evaluate_lab_runner_capabilities(
            "lab",
            &plan,
            &capabilities,
            LabRunnerGateMode::Automatic,
        );

        let LabRunnerGateDecision::Missing {
            reason,
            remediation,
            ..
        } = decision
        else {
            panic!("expected local fallback decision");
        };
        assert!(reason.contains("Local fallback"));
        assert!(reason.contains("playwright+browsers"));
        assert!(remediation.iter().any(|item| item.contains("run locally")));
    }
}
