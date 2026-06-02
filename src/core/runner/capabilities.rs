use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::server::{self, SshClient};

use super::Runner;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerCapabilityPreflight {
    pub command: String,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub required_components: Vec<String>,
    pub required_env: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerCapabilityPlan {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerCapabilityContract {
    pub command: &'static str,
    pub required_tools: Vec<RunnerRequiredTool>,
    pub requires_playwright: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerGateMode {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRunnerGateDecision {
    Eligible,
    Missing {
        runner_id: String,
        command: &'static str,
        missing_tools: Vec<RunnerRequiredTool>,
        reason: String,
        remediation: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RunnerRequiredTool {
    Homeboy,
    Cargo,
    Git,
    Node,
    Npm,
    Pnpm,
    Php,
    Composer,
    Docker,
    Playwright,
}

pub fn lab_runner_capability_plan(
    contract: LabRunnerCapabilityContract,
) -> LabRunnerCapabilityPlan {
    let mut required_tools = vec![RunnerRequiredTool::Git];
    for tool in contract.required_tools {
        push_unique(&mut required_tools, tool);
    }

    if contract.requires_playwright {
        push_unique(&mut required_tools, RunnerRequiredTool::Playwright);
    }

    LabRunnerCapabilityPlan {
        command: contract.command,
        required_tools,
    }
}

pub fn lab_runner_capability_preflight(
    contract: LabRunnerCapabilityContract,
) -> RunnerCapabilityPreflight {
    lab_runner_capability_plan(contract).into()
}

pub fn evaluate_lab_runner_capabilities_for_runner(
    runner: &Runner,
    plan: &LabRunnerCapabilityPlan,
    mode: LabRunnerGateMode,
) -> Result<LabRunnerGateDecision> {
    let capabilities = RunnerCapabilitySnapshot::from_runner_probe(runner)?;
    Ok(evaluate_lab_runner_capabilities(
        &runner.id,
        plan,
        &capabilities,
        mode,
    ))
}

pub(crate) fn runner_capability_snapshot(runner: &Runner) -> Result<RunnerCapabilitySnapshot> {
    RunnerCapabilitySnapshot::from_runner_probe(runner)
}

pub(crate) fn validate_runner_capability_preflight(
    runner_id: &str,
    preflight: &RunnerCapabilityPreflight,
    capabilities: &RunnerCapabilitySnapshot,
    request_env: &HashMap<String, String>,
) -> Result<()> {
    let missing_tools = preflight
        .required_tools
        .iter()
        .copied()
        .filter(|tool| !capabilities.has_tool(*tool))
        .collect::<Vec<_>>();
    let missing_components = preflight
        .required_components
        .iter()
        .filter(|component| !capabilities.components.contains(component.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let missing_env = preflight
        .required_env
        .iter()
        .filter(|name| {
            request_env
                .get(name.as_str())
                .is_none_or(|value| value.is_empty())
        })
        .cloned()
        .collect::<Vec<_>>();

    if missing_tools.is_empty() && missing_components.is_empty() && missing_env.is_empty() {
        return Ok(());
    }

    let mut missing = Vec::new();
    if !missing_tools.is_empty() {
        missing.push(format!(
            "tools: {}",
            missing_tools
                .iter()
                .map(|tool| tool.id())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !missing_components.is_empty() {
        missing.push(format!("components: {}", missing_components.join(", ")));
    }
    if !missing_env.is_empty() {
        missing.push(format!("environment: {}", missing_env.join(", ")));
    }

    let command = if preflight.command.is_empty() {
        "runner command"
    } else {
        preflight.command.as_str()
    };
    let mut remediation = missing_tools
        .iter()
        .map(|tool| tool.remediation().to_string())
        .collect::<Vec<_>>();
    remediation.extend(missing_components.iter().map(|component| {
        format!("Register component '{component}' on the runner capability profile or choose a runner with that component.")
    }));
    remediation.extend(missing_env.iter().map(|name| {
        format!("Set required environment variable '{name}' on the runner or pass it with the runner exec request.")
    }));
    remediation.push(
        "Remote execution was not started; fix the runner capability parity issue and retry."
            .to_string(),
    );

    Err(Error::validation_invalid_argument(
        "runner_capabilities",
        format!(
            "Runner '{runner_id}' is missing required capability parity for `{command}`: {}.",
            missing.join("; ")
        ),
        Some(runner_id.to_string()),
        Some(remediation),
    ))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RunnerCapabilitySnapshot {
    tools: BTreeSet<RunnerRequiredTool>,
    components: BTreeSet<String>,
}

impl RunnerCapabilitySnapshot {
    fn from_runner_probe(runner: &Runner) -> Result<Self> {
        let client = Self::ssh_client_for_runner(runner)?;
        let mut tools = BTreeSet::new();
        for tool in [
            RunnerRequiredTool::Homeboy,
            RunnerRequiredTool::Cargo,
            RunnerRequiredTool::Git,
            RunnerRequiredTool::Node,
            RunnerRequiredTool::Npm,
            RunnerRequiredTool::Pnpm,
            RunnerRequiredTool::Php,
            RunnerRequiredTool::Composer,
            RunnerRequiredTool::Docker,
            RunnerRequiredTool::Playwright,
        ] {
            if Self::tool_available(runner, &client, tool) {
                tools.insert(tool);
            }
        }

        Ok(Self {
            tools,
            components: configured_runner_components(runner),
        })
    }

    fn has_tool(&self, tool: RunnerRequiredTool) -> bool {
        self.tools.contains(&tool)
    }

    fn tool_available(runner: &Runner, client: &SshClient, tool: RunnerRequiredTool) -> bool {
        let command = match tool {
            RunnerRequiredTool::Homeboy => {
                runner.settings.homeboy_path.as_deref().unwrap_or("homeboy")
            }
            RunnerRequiredTool::Cargo => "cargo",
            RunnerRequiredTool::Git => "git",
            RunnerRequiredTool::Node => "node",
            RunnerRequiredTool::Npm => concat!("n", "pm"),
            RunnerRequiredTool::Pnpm => concat!("p", "n", "pm"),
            RunnerRequiredTool::Php => concat!("p", "hp"),
            RunnerRequiredTool::Composer => concat!("com", "poser"),
            RunnerRequiredTool::Docker => "docker",
            RunnerRequiredTool::Playwright => return Self::playwright_ready(client),
        };
        client
            .execute(&format!(
                "command -v {} >/dev/null 2>&1",
                shell::quote_arg(command)
            ))
            .success
    }

    fn playwright_ready(client: &SshClient) -> bool {
        let playwright = client
            .execute("command -v playwright >/dev/null 2>&1")
            .success;
        if !playwright {
            return false;
        }
        let browser_cache = "for d in \"${PLAYWRIGHT_BROWSERS_PATH:-}\" \"$HOME/Library/Caches/ms-playwright\" \"$HOME/.cache/ms-playwright\"; do [ -n \"$d\" ] && [ -d \"$d\" ] && find \"$d\" -mindepth 1 -maxdepth 1 2>/dev/null | grep -q . && exit 0; done; exit 1";
        client.execute(browser_cache).success
    }

    fn ssh_client_for_runner(runner: &Runner) -> Result<SshClient> {
        let server_id = runner.server_id.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "server_id",
                "SSH runners require server_id",
                Some(runner.id.clone()),
                None,
            )
        })?;
        let server = server::load(server_id)?;
        let mut client = SshClient::from_server(&server, server_id)?;
        client.env.extend(runner.env.clone());
        Ok(client)
    }
}

fn evaluate_lab_runner_capabilities(
    runner_id: &str,
    plan: &LabRunnerCapabilityPlan,
    capabilities: &RunnerCapabilitySnapshot,
    mode: LabRunnerGateMode,
) -> LabRunnerGateDecision {
    let missing_tools = plan
        .required_tools
        .iter()
        .copied()
        .filter(|tool| !capabilities.has_tool(*tool))
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

fn configured_runner_components(runner: &Runner) -> BTreeSet<String> {
    let Some(value) = runner.resources.get("components") else {
        return BTreeSet::new();
    };
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_string))
            })
            .collect(),
        Value::Object(map) => map.keys().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn tool_list(tools: &[RunnerRequiredTool]) -> String {
    tools
        .iter()
        .map(|tool| tool.id())
        .collect::<Vec<_>>()
        .join(", ")
}

impl From<LabRunnerCapabilityPlan> for RunnerCapabilityPreflight {
    fn from(plan: LabRunnerCapabilityPlan) -> Self {
        Self {
            command: plan.command.to_string(),
            required_tools: plan.required_tools,
            required_components: Vec::new(),
            required_env: Vec::new(),
        }
    }
}

impl RunnerCapabilityPreflight {
    pub(crate) fn is_empty(&self) -> bool {
        self.required_tools.is_empty()
            && self.required_components.is_empty()
            && self.required_env.is_empty()
    }
}

impl RunnerRequiredTool {
    pub fn id(self) -> &'static str {
        match self {
            RunnerRequiredTool::Homeboy => "homeboy",
            RunnerRequiredTool::Cargo => "cargo",
            RunnerRequiredTool::Git => "git",
            RunnerRequiredTool::Node => "node",
            RunnerRequiredTool::Npm => concat!("n", "pm"),
            RunnerRequiredTool::Pnpm => concat!("p", "n", "pm"),
            RunnerRequiredTool::Php => concat!("p", "hp"),
            RunnerRequiredTool::Composer => concat!("com", "poser"),
            RunnerRequiredTool::Docker => "docker",
            RunnerRequiredTool::Playwright => "playwright+browsers",
        }
    }

    pub(crate) fn remediation(self) -> &'static str {
        match self {
            RunnerRequiredTool::Homeboy => {
                "Install Homeboy on the runner and ensure the configured homeboy_path works."
            }
            RunnerRequiredTool::Cargo => {
                "Install Rust/Cargo and ensure cargo is on the runner PATH."
            }
            RunnerRequiredTool::Git => "Install git and ensure it is on the runner PATH.",
            RunnerRequiredTool::Node => "Install Node.js and ensure node is on the runner PATH.",
            RunnerRequiredTool::Npm => concat!(
                "Install n",
                "pm with Node.js and ensure n",
                "pm is on the runner PATH."
            ),
            RunnerRequiredTool::Pnpm => concat!(
                "Install p",
                "n",
                "pm for repositories with p",
                "n",
                "pm-lock.yaml."
            ),
            RunnerRequiredTool::Php => {
                concat!("Install P", "HP for repositories with com", "poser.json.")
            }
            RunnerRequiredTool::Composer => concat!(
                "Install Com",
                "poser for repositories with com",
                "poser.json."
            ),
            RunnerRequiredTool::Docker => {
                "Install and start Docker for container-backed repositories."
            }
            RunnerRequiredTool::Playwright => {
                "Install Playwright CLI and browser binaries on the runner."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::RunnerKind;
    use crate::core::server::{RunnerPolicy, RunnerSettings};

    fn ssh_runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("srv".to_string()),
            workspace_root: Some("/srv/homeboy".to_string()),
            settings: RunnerSettings {
                daemon: true,
                ..Default::default()
            },
            env: Default::default(),
            resources: Default::default(),
            policy: RunnerPolicy::default(),
        }
    }

    #[test]
    fn lab_runner_capability_plan_detects_project_tool_needs() {
        let plan = lab_runner_capability_plan(LabRunnerCapabilityContract {
            command: "lint",
            required_tools: vec![
                RunnerRequiredTool::Node,
                RunnerRequiredTool::Npm,
                RunnerRequiredTool::Node,
                RunnerRequiredTool::Pnpm,
                RunnerRequiredTool::Php,
                RunnerRequiredTool::Composer,
                RunnerRequiredTool::Docker,
            ],
            requires_playwright: false,
        });

        assert_eq!(plan.command, "lint");
        assert_eq!(
            plan.required_tools,
            vec![
                RunnerRequiredTool::Git,
                RunnerRequiredTool::Node,
                RunnerRequiredTool::Npm,
                RunnerRequiredTool::Pnpm,
                RunnerRequiredTool::Php,
                RunnerRequiredTool::Composer,
                RunnerRequiredTool::Docker,
            ]
        );
    }

    #[test]
    fn lab_runner_capability_preflight_uses_core_required_tools() {
        let preflight = lab_runner_capability_preflight(LabRunnerCapabilityContract {
            command: "test",
            required_tools: vec![RunnerRequiredTool::Node, RunnerRequiredTool::Pnpm],
            requires_playwright: false,
        });

        assert_eq!(preflight.command, "test");
        assert_eq!(
            preflight.required_tools,
            vec![
                RunnerRequiredTool::Git,
                RunnerRequiredTool::Node,
                RunnerRequiredTool::Pnpm,
            ]
        );
    }

    #[test]
    fn lab_runner_gate_allows_capable_runner() {
        let plan = LabRunnerCapabilityPlan {
            command: "lint",
            required_tools: vec![
                RunnerRequiredTool::Git,
                RunnerRequiredTool::Node,
                RunnerRequiredTool::Pnpm,
            ],
        };
        let capabilities = RunnerCapabilitySnapshot {
            tools: [
                RunnerRequiredTool::Git,
                RunnerRequiredTool::Node,
                RunnerRequiredTool::Pnpm,
            ]
            .into_iter()
            .collect(),
            components: BTreeSet::new(),
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
            required_tools: vec![RunnerRequiredTool::Git, RunnerRequiredTool::Pnpm],
        };
        let capabilities = RunnerCapabilitySnapshot {
            tools: [RunnerRequiredTool::Git].into_iter().collect(),
            components: BTreeSet::new(),
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
        assert_eq!(missing_tools, vec![RunnerRequiredTool::Pnpm]);
        assert!(reason.contains("Lab offload runner 'lab'"));
        assert!(reason.contains(concat!("p", "n", "pm")));
        assert!(remediation
            .iter()
            .any(|item| item.contains("omit --runner")));
    }

    #[test]
    fn lab_runner_gate_reports_local_fallback_for_auto_runner() {
        let plan = LabRunnerCapabilityPlan {
            command: "trace",
            required_tools: vec![RunnerRequiredTool::Git, RunnerRequiredTool::Playwright],
        };
        let capabilities = RunnerCapabilitySnapshot {
            tools: [RunnerRequiredTool::Git].into_iter().collect(),
            components: BTreeSet::new(),
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

    #[test]
    fn runner_capability_preflight_reports_missing_tools_components_and_env() {
        let preflight = RunnerCapabilityPreflight {
            command: "test".to_string(),
            required_tools: vec![RunnerRequiredTool::Git, RunnerRequiredTool::Pnpm],
            required_components: vec!["nodejs".to_string(), "wordpress".to_string()],
            required_env: vec!["HOMEBOY_TOKEN".to_string()],
        };
        let capabilities = RunnerCapabilitySnapshot {
            tools: [RunnerRequiredTool::Git].into_iter().collect(),
            components: ["nodejs".to_string()].into_iter().collect(),
        };

        let err =
            validate_runner_capability_preflight("lab", &preflight, &capabilities, &HashMap::new())
                .expect_err("missing capability parity");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains(concat!("tools: p", "n", "pm")));
        assert!(err.message.contains("components: wordpress"));
        assert!(err.message.contains("environment: HOMEBOY_TOKEN"));
        let tried = err
            .details
            .get("tried")
            .and_then(Value::as_array)
            .expect("remediation details");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Remote execution was not started"))));
    }

    #[test]
    fn runner_capability_preflight_accepts_matching_requirements() {
        let preflight = RunnerCapabilityPreflight {
            command: "lint".to_string(),
            required_tools: vec![RunnerRequiredTool::Git, RunnerRequiredTool::Node],
            required_components: vec!["nodejs".to_string()],
            required_env: vec!["HOMEBOY_TOKEN".to_string()],
        };
        let capabilities = RunnerCapabilitySnapshot {
            tools: [RunnerRequiredTool::Git, RunnerRequiredTool::Node]
                .into_iter()
                .collect(),
            components: ["nodejs".to_string()].into_iter().collect(),
        };
        let mut env = HashMap::new();
        env.insert("HOMEBOY_TOKEN".to_string(), "present".to_string());

        validate_runner_capability_preflight("lab", &preflight, &capabilities, &env)
            .expect("capability parity passes");
    }

    #[test]
    fn configured_runner_components_accept_arrays_and_maps() {
        let mut runner = ssh_runner();
        runner.resources.insert(
            "components".to_string(),
            serde_json::json!(["nodejs", { "id": "wordpress" }]),
        );
        assert_eq!(
            configured_runner_components(&runner),
            ["nodejs".to_string(), "wordpress".to_string()]
                .into_iter()
                .collect()
        );

        runner.resources.insert(
            "components".to_string(),
            serde_json::json!({ "rust": true, "php": true }),
        );
        assert_eq!(
            configured_runner_components(&runner),
            ["php".to_string(), "rust".to_string()]
                .into_iter()
                .collect()
        );
    }
}
