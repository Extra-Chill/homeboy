use super::capabilities::RunnerRequiredTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerToolSpec {
    pub tool: Option<RunnerRequiredTool>,
    pub id: &'static str,
    pub capability_id: &'static str,
    pub check_id: &'static str,
    pub command: &'static str,
    pub version_args: &'static [&'static str],
    pub required: bool,
    pub remediation: &'static str,
    pub capability_remediation: &'static str,
}

pub struct RunnerToolRegistry;

impl RunnerToolRegistry {
    pub const REQUIRED_TOOLS: &'static [RunnerRequiredTool] = &[
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
    ];

    pub const DOCTOR_TOOLS: &'static [RunnerToolSpec] = &[
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Homeboy),
            id: "homeboy",
            capability_id: "homeboy",
            check_id: "homeboy",
            command: "homeboy",
            version_args: &["--version"],
            required: true,
            remediation: "Install Homeboy on the remote runner or configure runner.homeboy_path/server.env.PATH",
            capability_remediation: "Install Homeboy on the runner and ensure the configured homeboy_path works.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Cargo),
            id: "cargo",
            capability_id: "cargo",
            check_id: "tool.cargo",
            command: "cargo",
            version_args: &["--version"],
            required: false,
            remediation: "Install Rust/Cargo and ensure cargo is on PATH",
            capability_remediation: "Install Rust/Cargo and ensure cargo is on the runner PATH.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Git),
            id: "git",
            capability_id: "git",
            check_id: "tool.git",
            command: "git",
            version_args: &["--version"],
            required: true,
            remediation: "Install git and ensure it is on PATH",
            capability_remediation: "Install git and ensure it is on the runner PATH.",
        },
        RunnerToolSpec {
            tool: None,
            id: "gh",
            capability_id: "gh",
            check_id: "tool.github_cli",
            command: "gh",
            version_args: &["--version"],
            required: false,
            remediation: "Install GitHub CLI (`gh`) for PR and issue workflows",
            capability_remediation: "Install GitHub CLI (`gh`) for PR and issue workflows",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Node),
            id: "node",
            capability_id: "node",
            check_id: "tool.node",
            command: "node",
            version_args: &["--version"],
            required: false,
            remediation: "Install Node.js for JavaScript/TypeScript components",
            capability_remediation: "Install Node.js and ensure node is on the runner PATH.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Npm),
            id: "npm",
            capability_id: "npm",
            check_id: "tool.npm",
            command: "npm",
            version_args: &["--version"],
            required: false,
            remediation: "Install npm with Node.js",
            capability_remediation: "Install npm with Node.js and ensure npm is on the runner PATH.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Pnpm),
            id: "pnpm",
            capability_id: "pnpm",
            check_id: "tool.pnpm",
            command: "pnpm",
            version_args: &["--version"],
            required: false,
            remediation: "Install pnpm for repos that use pnpm-lock.yaml",
            capability_remediation: "Install pnpm for repositories with pnpm-lock.yaml.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Php),
            id: "php",
            capability_id: "php",
            check_id: "tool.php",
            command: "php",
            version_args: &["--version"],
            required: false,
            remediation: "Install PHP for WordPress/PHP components",
            capability_remediation: "Install PHP for repositories with composer.json.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Composer),
            id: "composer",
            capability_id: "composer",
            check_id: "tool.composer",
            command: "composer",
            version_args: &["--version"],
            required: false,
            remediation: "Install Composer for PHP dependencies",
            capability_remediation: "Install Composer for repositories with composer.json.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Docker),
            id: "docker",
            capability_id: "docker",
            check_id: "tool.docker",
            command: "docker",
            version_args: &["--version"],
            required: false,
            remediation: "Install and start Docker for container-backed rigs",
            capability_remediation: "Install and start Docker for container-backed repositories.",
        },
        RunnerToolSpec {
            tool: Some(RunnerRequiredTool::Playwright),
            id: "playwright",
            capability_id: "playwright+browsers",
            check_id: "tool.playwright",
            command: "playwright",
            version_args: &["--version"],
            required: false,
            remediation: "Install Playwright CLI and browsers for browser traces",
            capability_remediation: "Install Playwright CLI and browser binaries on the runner.",
        },
    ];

    pub fn required_tools() -> &'static [RunnerRequiredTool] {
        Self::REQUIRED_TOOLS
    }

    pub fn doctor_tools() -> &'static [RunnerToolSpec] {
        Self::DOCTOR_TOOLS
    }

    pub fn spec_for_required_tool(tool: RunnerRequiredTool) -> Option<&'static RunnerToolSpec> {
        Self::DOCTOR_TOOLS
            .iter()
            .find(|spec| spec.tool == Some(tool))
    }
}
