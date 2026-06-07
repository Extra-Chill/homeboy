use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::core::runner::{self, Runner, RunnerKind, RunnerTunnelMode};
use homeboy::core::server::{self, Server, SshClient};
use serde::Serialize;

use crate::commands::CmdResult;

#[path = "doctor/extension_parity.rs"]
mod extension_parity;

pub use types::RunnerDoctorOutput;

#[derive(Debug, Default)]
pub struct RunnerDoctorOptions {
    pub path: Option<String>,
    pub extensions: Vec<String>,
    pub required_tools: Vec<String>,
}

pub fn run(runner_id: &str) -> CmdResult<RunnerDoctorOutput> {
    run_with_options(runner_id, RunnerDoctorOptions::default())
}

pub fn run_with_options(
    runner_id: &str,
    options: RunnerDoctorOptions,
) -> CmdResult<RunnerDoctorOutput> {
    let target = target::resolve(runner_id)?;
    let report = match &target {
        target::RunnerTarget::Local { id, runner } => local::report(id, runner.as_ref(), &options),
        target::RunnerTarget::Ssh {
            id,
            runner,
            server,
            client,
        } => remote::report(id, runner, server, client, &options),
    };
    Ok((report, 0))
}

mod types {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum RunnerDoctorStatus {
        Ok,
        #[serde(rename = "warn")]
        Warning,
        Error,
    }

    #[derive(Debug, Serialize)]
    pub struct RunnerDoctorOutput {
        pub command: &'static str,
        pub runner_id: String,
        pub runner: RunnerTargetSummary,
        pub status: RunnerDoctorStatus,
        pub capabilities: RunnerCapabilities,
        pub resources: RunnerResources,
        pub checks: Vec<RunnerCheck>,
    }

    #[derive(Debug, Serialize)]
    pub struct RunnerTargetSummary {
        #[serde(rename = "type")]
        pub target_type: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub registry: Option<RunnerRegistrySummary>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub server: Option<RunnerServerSummary>,
    }

    #[derive(Debug, Serialize)]
    pub struct RunnerRegistrySummary {
        pub id: String,
        pub kind: RunnerKind,
    }

    #[derive(Debug, Serialize)]
    pub struct RunnerServerSummary {
        pub id: String,
        pub host: String,
        pub user: String,
        pub port: u16,
        pub is_localhost: bool,
    }

    #[derive(Debug, Default, Serialize)]
    pub struct RunnerCapabilities {
        pub local_execution: bool,
        pub ssh_execution: bool,
        pub git: bool,
        pub github_cli: bool,
        pub node: bool,
        pub npm: bool,
        pub pnpm: bool,
        pub php: bool,
        pub composer: bool,
        pub docker: bool,
        pub playwright: bool,
        pub browser_ready: bool,
        pub workspace_writable: bool,
        pub artifact_store_available: bool,
    }

    #[derive(Debug, Default, Serialize)]
    pub struct RunnerResources {
        pub homeboy: HomeboyProbe,
        pub system: SystemProbe,
        pub cpu: CpuProbe,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub memory: Option<MemoryProbe>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub disk: Option<DiskProbe>,
        pub workspace_root: String,
        pub artifact_root: String,
        pub tools: BTreeMap<String, ToolProbe>,
    }

    #[derive(Debug, Default, Serialize)]
    pub struct HomeboyProbe {
        pub version: String,
        pub path: Option<String>,
    }

    #[derive(Debug, Default, Serialize)]
    pub struct SystemProbe {
        pub os: String,
        pub arch: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub kernel: Option<String>,
    }

    #[derive(Debug, Default, Serialize)]
    pub struct CpuProbe {
        pub count: usize,
    }

    #[derive(Debug, Serialize)]
    pub struct MemoryProbe {
        pub total_mb: u64,
        pub available_mb: Option<u64>,
    }

    #[derive(Debug, Serialize)]
    pub struct DiskProbe {
        pub path: String,
        pub total_mb: u64,
        pub available_mb: u64,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct ToolProbe {
        pub available: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
    }

    #[derive(Debug, Serialize)]
    pub struct RunnerCheck {
        pub id: String,
        pub status: RunnerDoctorStatus,
        pub message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub remediation: Option<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        pub details: BTreeMap<String, String>,
    }
}

mod target {
    use super::*;

    pub enum RunnerTarget {
        Local {
            id: String,
            runner: Option<Runner>,
        },
        Ssh {
            id: String,
            runner: Runner,
            server: Server,
            client: SshClient,
        },
    }

    pub fn resolve(runner_id: &str) -> homeboy::core::Result<RunnerTarget> {
        match runner::load(runner_id) {
            Ok(runner) => from_registry(runner_id, runner),
            Err(_) if is_local_runner_id(runner_id) => Ok(RunnerTarget::Local {
                id: runner_id.to_string(),
                runner: None,
            }),
            Err(err) => Err(err),
        }
    }

    fn from_registry(runner_id: &str, runner: Runner) -> homeboy::core::Result<RunnerTarget> {
        match runner.kind {
            RunnerKind::Local => Ok(RunnerTarget::Local {
                id: runner_id.to_string(),
                runner: Some(runner),
            }),
            RunnerKind::Ssh => {
                let server_id = runner.server_id.as_deref().ok_or_else(|| {
                    homeboy::core::Error::validation_invalid_argument(
                        "server_id",
                        "SSH runners require server_id",
                        None,
                        None,
                    )
                })?;
                let server = server::load(server_id)?;
                let mut client = SshClient::from_server(&server, server_id)?;
                client.env.extend(runner.env.clone());
                Ok(RunnerTarget::Ssh {
                    id: runner_id.to_string(),
                    runner,
                    server,
                    client,
                })
            }
        }
    }

    fn is_local_runner_id(runner_id: &str) -> bool {
        matches!(runner_id, "local" | "localhost" | "self")
    }
}

mod local {
    use super::*;
    use types::*;

    pub fn report(
        runner_id: &str,
        runner: Option<&Runner>,
        options: &RunnerDoctorOptions,
    ) -> RunnerDoctorOutput {
        let workspace_root = runner
            .and_then(|runner| runner.workspace_root.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let artifact_root = crate::core::paths::artifact_root()
            .unwrap_or_else(|_| workspace_root.join(".homeboy-artifacts"));
        let mut checks = Vec::new();
        let mut tools = BTreeMap::new();

        let homeboy = HomeboyProbe {
            version: env!("CARGO_PKG_VERSION").to_string(),
            path: runner
                .and_then(|runner| runner.settings.homeboy_path.clone())
                .or_else(|| env::current_exe().ok().map(common::display_path)),
        };
        checks.push(checks::ok(
            "homeboy",
            format!("Homeboy {} is running", homeboy.version),
            None,
        ));

        let system = SystemProbe {
            os: env::consts::OS.to_string(),
            arch: env::consts::ARCH.to_string(),
            kernel: common::local_command_line("uname", &["-r"]),
        };
        checks.push(checks::ok(
            "system",
            format!("{} {} runner detected", system.os, system.arch),
            None,
        ));

        let cpu = CpuProbe {
            count: std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(1),
        };
        checks.push(checks::ok(
            "cpu",
            format!("{} CPU cores detected", cpu.count),
            None,
        ));

        let memory = probes::local_memory_probe();
        checks.push(match &memory {
            Some(memory) => checks::ok(
                "memory",
                format!("{} MB RAM detected", memory.total_mb),
                None,
            ),
            None => checks::warning(
                "memory",
                "RAM totals could not be detected".to_string(),
                Some("Install platform tools such as sysctl/vm_stat or run on Linux with /proc/meminfo".to_string()),
            ),
        });

        let disk = probes::local_disk_probe(&workspace_root);
        checks.push(match &disk {
            Some(disk) => checks::ok(
                "disk.workspace_root",
                format!("{} MB available at workspace root", disk.available_mb),
                None,
            ),
            None => checks::warning(
                "disk.workspace_root",
                "Workspace disk capacity could not be detected".to_string(),
                Some("Ensure df/statvfs is available for the workspace filesystem".to_string()),
            ),
        });

        for spec in probes::tool_specs() {
            let probe = probes::local_tool_probe(spec.command, spec.version_args);
            checks.push(checks::tool_check(*spec, &probe));
            tools.insert(spec.id.to_string(), probe);
        }

        for command in normalized_required_tools(&options.required_tools) {
            let probe = probes::local_tool_probe(&command, &[]);
            checks.push(checks::required_tool_check(&command, &probe));
            tools.entry(command).or_insert(probe);
        }

        let playwright = probes::tool_available(&tools, "playwright");
        let browser_ready = probes::local_browser_ready();
        checks.push(checks::playwright_check(playwright, browser_ready));

        let workspace_writable = probes::local_path_writable(&workspace_root);
        checks.push(checks::path_writable_check(
            "workspace.writable",
            workspace_writable,
            &workspace_root,
            "Make the workspace root writable by the runner user or choose a writable checkout path",
        ));

        let artifact_store_available = probes::local_path_or_parent_writable(&artifact_root);
        checks.push(checks::path_writable_check(
            "artifact_store.available",
            artifact_store_available,
            &artifact_root,
            "Create the artifact root or configure HOMEBOY_ARTIFACT_ROOT to a writable directory",
        ));

        let homeboy_command = homeboy.path.as_deref().unwrap_or("homeboy");
        for extension_id in normalized_extension_ids(&options.extensions) {
            checks.push(extension_parity::local_check(
                homeboy_command,
                options.path.as_deref(),
                &extension_id,
            ));
        }

        let capabilities = probes::capabilities_from(
            &tools,
            true,
            false,
            playwright,
            browser_ready,
            workspace_writable,
            artifact_store_available,
        );
        let resources = RunnerResources {
            homeboy,
            system,
            cpu,
            memory,
            disk,
            workspace_root: common::display_path(workspace_root),
            artifact_root: common::display_path(artifact_root),
            tools,
        };

        RunnerDoctorOutput {
            command: "runner.doctor",
            runner_id: runner_id.to_string(),
            runner: runner_summary("local", runner, None),
            status: checks::overall_status(&checks),
            capabilities,
            resources,
            checks,
        }
    }
}

mod remote {
    use super::*;
    use types::*;

    pub fn report(
        runner_id: &str,
        runner: &Runner,
        server: &Server,
        client: &SshClient,
        options: &RunnerDoctorOptions,
    ) -> RunnerDoctorOutput {
        let workspace_root = runner
            .workspace_root
            .clone()
            .unwrap_or_else(|| ".".to_string());
        let artifact_root = default_artifact_root(client);
        let mut checks = Vec::new();
        let mut tools = BTreeMap::new();

        checks.push(match client.execute("printf ok") {
            output if output.success && output.stdout.trim() == "ok" => checks::ok(
                "ssh.execution",
                format!("SSH runner {} is reachable", runner_id),
                None,
            ),
            output => checks::error(
                "ssh.execution",
                format!("SSH runner {} is not reachable", runner_id),
                Some("Run `homeboy server status <server-id>` and verify host, user, port, identity_file, and network access".to_string()),
                common::detail_map(&[("stderr", output.stderr.trim()), ("stdout", output.stdout.trim())]),
            ),
        });

        let homeboy_command = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
        let local_homeboy_version = env!("CARGO_PKG_VERSION");
        let homeboy = HomeboyProbe {
            version: common::remote_line(
                client,
                &format!(
                    "{} --version | awk '{{print $2}}'",
                    common::shell_word(homeboy_command)
                ),
            )
            .unwrap_or_else(|| "unknown".to_string()),
            path: runner
                .settings
                .homeboy_path
                .clone()
                .or_else(|| common::remote_line(client, "command -v homeboy")),
        };
        if let Some(check) = checks::homeboy_version_skew_check(
            local_homeboy_version,
            &homeboy.version,
            runner_id,
            &server.id,
        ) {
            checks.push(check);
        }
        checks.push(if homeboy.path.is_some() {
            checks::ok(
                "homeboy",
                "Homeboy is available on the remote runner".to_string(),
                None,
            )
        } else {
            checks::warning(
                "homeboy",
                "Homeboy was not found on the remote runner PATH".to_string(),
                Some("Install Homeboy on the remote runner or configure runner.homeboy_path/server.env.PATH".to_string()),
            )
        });

        let system = SystemProbe {
            os: common::remote_line(client, "uname -s").unwrap_or_else(|| "unknown".to_string()),
            arch: common::remote_line(client, "uname -m").unwrap_or_else(|| "unknown".to_string()),
            kernel: common::remote_line(client, "uname -r"),
        };
        checks.push(checks::ok(
            "system",
            format!("{} {} runner detected", system.os, system.arch),
            None,
        ));

        let cpu = CpuProbe {
            count: common::remote_line(client, "getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(1),
        };
        checks.push(checks::ok(
            "cpu",
            format!("{} CPU cores detected", cpu.count),
            None,
        ));

        let memory = probes::remote_memory_probe(client);
        checks.push(match &memory {
            Some(memory) => checks::ok(
                "memory",
                format!("{} MB RAM detected", memory.total_mb),
                None,
            ),
            None => checks::warning(
                "memory",
                "RAM totals could not be detected".to_string(),
                Some(
                    "Ensure /proc/meminfo or sysctl is available on the remote runner".to_string(),
                ),
            ),
        });

        let disk = probes::remote_disk_probe(client, &workspace_root);
        checks.push(match &disk {
            Some(disk) => checks::ok(
                "disk.workspace_root",
                format!("{} MB available at workspace root", disk.available_mb),
                None,
            ),
            None => checks::warning(
                "disk.workspace_root",
                "Workspace disk capacity could not be detected".to_string(),
                Some("Ensure df is available on the remote runner".to_string()),
            ),
        });

        for spec in probes::tool_specs() {
            let probe = probes::remote_tool_probe(client, spec.command, spec.version_args);
            checks.push(checks::tool_check(*spec, &probe));
            tools.insert(spec.id.to_string(), probe);
        }

        for command in normalized_required_tools(&options.required_tools) {
            let probe = probes::remote_tool_probe(client, &command, &[]);
            checks.push(checks::required_tool_check(&command, &probe));
            tools.entry(command).or_insert(probe);
        }

        let playwright = probes::tool_available(&tools, "playwright");
        let browser_ready = probes::remote_browser_ready(client);
        checks.push(checks::playwright_check(playwright, browser_ready));

        let workspace_writable = probes::remote_path_writable(client, &workspace_root);
        checks.push(checks::path_writable_check(
            "workspace.writable",
            workspace_writable,
            Path::new(&workspace_root),
            "Make the remote workspace root writable by the runner user",
        ));

        let artifact_store_available =
            probes::remote_artifact_store_available(client, &artifact_root);
        checks.push(checks::path_writable_check(
            "artifact_store.available",
            artifact_store_available,
            Path::new(&artifact_root),
            "Create the artifact root or configure HOMEBOY_ARTIFACT_ROOT to a writable directory",
        ));

        checks.extend(probes::connected_daemon_exec_checks(
            runner_id,
            &workspace_root,
        ));

        for extension_id in normalized_extension_ids(&options.extensions) {
            checks.push(extension_parity::remote_check(
                client,
                homeboy_command,
                options.path.as_deref(),
                &extension_id,
            ));
        }

        let capabilities = probes::capabilities_from(
            &tools,
            false,
            true,
            playwright,
            browser_ready,
            workspace_writable,
            artifact_store_available,
        );
        let resources = RunnerResources {
            homeboy,
            system,
            cpu,
            memory,
            disk,
            workspace_root: workspace_root.clone(),
            artifact_root,
            tools,
        };

        RunnerDoctorOutput {
            command: "runner.doctor",
            runner_id: runner_id.to_string(),
            runner: runner_summary("ssh", Some(runner), Some(server)),
            status: checks::overall_status(&checks),
            capabilities,
            resources,
            checks,
        }
    }

    fn default_artifact_root(client: &SshClient) -> String {
        remote_home_dir(client)
            .and_then(|home| default_artifact_root_for_home(&home))
            .unwrap_or_else(|| "~/.local/share/homeboy/artifacts".to_string())
    }

    fn remote_home_dir(client: &SshClient) -> Option<String> {
        common::remote_line(
            client,
            "home=${HOME:-}; if [ -z \"$home\" ]; then home=$(getent passwd \"$(id -u)\" 2>/dev/null | cut -d: -f6); fi; if [ -z \"$home\" ]; then home=$(cd ~ 2>/dev/null && pwd -P); fi; [ -n \"$home\" ] && printf '%s\n' \"$home\"",
        )
    }

    pub(super) fn default_artifact_root_for_home(home: &str) -> Option<String> {
        let home = home.trim();
        if home.is_empty() {
            return None;
        }
        let home = home.trim_end_matches('/');
        if home.is_empty() {
            return Some("/.local/share/homeboy/artifacts".to_string());
        }
        Some(format!("{home}/.local/share/homeboy/artifacts"))
    }
}

mod probes {
    use super::*;
    use types::{DiskProbe, MemoryProbe, RunnerCapabilities, RunnerCheck, ToolProbe};

    #[derive(Clone, Copy)]
    pub struct ToolSpec {
        pub id: &'static str,
        pub check_id: &'static str,
        pub command: &'static str,
        pub version_args: &'static [&'static str],
        pub required: bool,
        pub remediation: &'static str,
    }

    pub fn tool_specs() -> &'static [ToolSpec] {
        &[
            ToolSpec {
                id: "git",
                check_id: "tool.git",
                command: "git",
                version_args: &["--version"],
                required: true,
                remediation: "Install git and ensure it is on PATH",
            },
            ToolSpec {
                id: "gh",
                check_id: "tool.github_cli",
                command: "gh",
                version_args: &["--version"],
                required: false,
                remediation: "Install GitHub CLI (`gh`) for PR and issue workflows",
            },
            ToolSpec {
                id: "node",
                check_id: "tool.node",
                command: "node",
                version_args: &["--version"],
                required: false,
                remediation: "Install Node.js for JavaScript/TypeScript components",
            },
            ToolSpec {
                id: "npm",
                check_id: "tool.npm",
                command: "npm",
                version_args: &["--version"],
                required: false,
                remediation: "Install npm with Node.js",
            },
            ToolSpec {
                id: "pnpm",
                check_id: "tool.pnpm",
                command: "pnpm",
                version_args: &["--version"],
                required: false,
                remediation: "Install pnpm for repos that use pnpm-lock.yaml",
            },
            ToolSpec {
                id: "php",
                check_id: "tool.php",
                command: "php",
                version_args: &["--version"],
                required: false,
                remediation: "Install PHP for WordPress/PHP components",
            },
            ToolSpec {
                id: "composer",
                check_id: "tool.composer",
                command: "composer",
                version_args: &["--version"],
                required: false,
                remediation: "Install Composer for PHP dependencies",
            },
            ToolSpec {
                id: "docker",
                check_id: "tool.docker",
                command: "docker",
                version_args: &["--version"],
                required: false,
                remediation: "Install and start Docker for container-backed rigs",
            },
            ToolSpec {
                id: "playwright",
                check_id: "tool.playwright",
                command: "playwright",
                version_args: &["--version"],
                required: false,
                remediation: "Install Playwright CLI and browsers for browser traces",
            },
        ]
    }

    pub fn capabilities_from(
        tools: &BTreeMap<String, ToolProbe>,
        local_execution: bool,
        ssh_execution: bool,
        playwright: bool,
        browser_ready: bool,
        workspace_writable: bool,
        artifact_store_available: bool,
    ) -> RunnerCapabilities {
        RunnerCapabilities {
            local_execution,
            ssh_execution,
            git: tool_available(tools, "git"),
            github_cli: tool_available(tools, "gh"),
            node: tool_available(tools, "node"),
            npm: tool_available(tools, "npm"),
            pnpm: tool_available(tools, "pnpm"),
            php: tool_available(tools, "php"),
            composer: tool_available(tools, "composer"),
            docker: tool_available(tools, "docker"),
            playwright,
            browser_ready,
            workspace_writable,
            artifact_store_available,
        }
    }

    pub fn tool_available(tools: &BTreeMap<String, ToolProbe>, id: &str) -> bool {
        tools.get(id).map(|tool| tool.available).unwrap_or(false)
    }

    pub fn local_tool_probe(command: &str, version_args: &[&str]) -> ToolProbe {
        let path = common::local_command_line(
            "sh",
            &[
                "-lc",
                &format!("command -v {}", common::shell_word(command)),
            ],
        );
        let Some(path) = path else {
            return ToolProbe {
                available: false,
                path: None,
                version: None,
                error: Some("not found on PATH".to_string()),
            };
        };
        let version = if version_args.is_empty() {
            None
        } else {
            Command::new(command)
                .args(version_args)
                .output()
                .ok()
                .and_then(|output| {
                    if output.status.success() {
                        common::first_output_line(&output.stdout, &output.stderr)
                    } else {
                        None
                    }
                })
        };
        ToolProbe {
            available: true,
            path: Some(path),
            version,
            error: None,
        }
    }

    pub fn remote_tool_probe(
        client: &SshClient,
        command: &str,
        version_args: &[&str],
    ) -> ToolProbe {
        let path = common::remote_line(
            client,
            &format!("command -v {}", common::shell_word(command)),
        );
        let Some(path) = path else {
            return ToolProbe {
                available: false,
                path: None,
                version: None,
                error: Some("not found on PATH".to_string()),
            };
        };
        let version = if version_args.is_empty() {
            None
        } else {
            let args = version_args
                .iter()
                .map(|arg| common::shell_word(arg))
                .collect::<Vec<_>>()
                .join(" ");
            common::remote_line(
                client,
                &format!(
                    "{} {} 2>&1 | sed -n '1p'",
                    common::shell_word(command),
                    args
                ),
            )
        };
        ToolProbe {
            available: true,
            path: Some(path),
            version,
            error: None,
        }
    }

    pub fn local_memory_probe() -> Option<MemoryProbe> {
        memory_from_proc_meminfo().or_else(memory_from_macos_sysctl)
    }

    pub fn remote_memory_probe(client: &SshClient) -> Option<MemoryProbe> {
        let total_mb = common::remote_line(
            client,
            "awk '/MemTotal:/ {print int($2/1024)}' /proc/meminfo 2>/dev/null || expr $(sysctl -n hw.memsize 2>/dev/null) / 1048576",
        )?
        .parse::<u64>()
        .ok()?;
        let available_mb = common::remote_line(
            client,
            "awk '/MemAvailable:/ {print int($2/1024)}' /proc/meminfo 2>/dev/null",
        )
        .and_then(|value| value.parse::<u64>().ok());
        Some(MemoryProbe {
            total_mb,
            available_mb,
        })
    }

    #[cfg(unix)]
    pub fn local_disk_probe(path: &Path) -> Option<DiskProbe> {
        let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        let stat = unsafe { stat.assume_init() };
        let block_size = u128::from(stat.f_frsize.max(1));
        let total_blocks = u128::from(stat.f_blocks);
        let available_blocks = u128::from(stat.f_bavail);
        Some(DiskProbe {
            path: common::display_path(path),
            total_mb: (total_blocks.saturating_mul(block_size) / 1024 / 1024)
                .try_into()
                .ok()?,
            available_mb: (available_blocks.saturating_mul(block_size) / 1024 / 1024)
                .try_into()
                .ok()?,
        })
    }

    #[cfg(not(unix))]
    pub fn local_disk_probe(_path: &Path) -> Option<DiskProbe> {
        None
    }

    pub fn remote_disk_probe(client: &SshClient, path: &str) -> Option<DiskProbe> {
        let line = common::remote_line(
            client,
            &format!(
                "df -Pk {} | awk 'NR==2 {{print $2 \" \" $4}}'",
                common::shell_word(path)
            ),
        )?;
        let mut parts = line.split_whitespace();
        let total_kb = parts.next()?.parse::<u64>().ok()?;
        let available_kb = parts.next()?.parse::<u64>().ok()?;
        Some(DiskProbe {
            path: path.to_string(),
            total_mb: total_kb / 1024,
            available_mb: available_kb / 1024,
        })
    }

    pub fn local_browser_ready() -> bool {
        browser_cache_candidates().into_iter().any(|path| {
            path.is_dir()
                && fs::read_dir(path)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false)
        })
    }

    pub fn remote_browser_ready(client: &SshClient) -> bool {
        let command = "for d in \"${PLAYWRIGHT_BROWSERS_PATH:-}\" \"$HOME/Library/Caches/ms-playwright\" \"$HOME/.cache/ms-playwright\"; do [ -n \"$d\" ] && [ -d \"$d\" ] && find \"$d\" -mindepth 1 -maxdepth 1 2>/dev/null | grep -q . && exit 0; done; exit 1";
        client.execute(command).success
    }

    #[cfg(unix)]
    pub fn local_path_writable(path: &Path) -> bool {
        let c_path = match std::ffi::CString::new(path.to_string_lossy().as_bytes()) {
            Ok(path) => path,
            Err(_) => return false,
        };
        unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
    }

    #[cfg(not(unix))]
    pub fn local_path_writable(path: &Path) -> bool {
        fs::metadata(path)
            .map(|metadata| !metadata.permissions().readonly())
            .unwrap_or(false)
    }

    pub fn local_path_or_parent_writable(path: &Path) -> bool {
        if path.exists() {
            local_path_writable(path)
        } else {
            path.parent().map(local_path_writable).unwrap_or(false)
        }
    }

    pub fn remote_path_writable(client: &SshClient, path: &str) -> bool {
        client
            .execute(&format!("test -w {}", common::shell_word(path)))
            .success
    }

    pub fn remote_artifact_store_available(client: &SshClient, path: &str) -> bool {
        client
            .execute(&format!(
                "if [ -e {0} ]; then test -w {0}; else test -w $(dirname {0}); fi",
                common::shell_word(path)
            ))
            .success
    }

    pub fn connected_daemon_exec_checks(runner_id: &str, workspace_root: &str) -> Vec<RunnerCheck> {
        let Ok(status) = runner::status(runner_id) else {
            return Vec::new();
        };
        if !status.connected {
            return Vec::new();
        }
        let Some(session) = status.session else {
            return Vec::new();
        };
        if session.mode != RunnerTunnelMode::DirectSsh {
            return Vec::new();
        }
        let Some(local_url) = session.local_url else {
            return vec![checks::error(
                "daemon.exec",
                "Connected direct runner session is missing its local daemon URL".to_string(),
                Some(format!(
                    "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}`"
                )),
                BTreeMap::new(),
            )];
        };

        vec![daemon_exec_check(runner_id, workspace_root, &local_url)]
    }

    pub(super) fn daemon_exec_check(
        runner_id: &str,
        workspace_root: &str,
        local_url: &str,
    ) -> RunnerCheck {
        let mut details = BTreeMap::new();
        details.insert("url".to_string(), local_url.to_string());
        details.insert("cwd".to_string(), workspace_root.to_string());
        let client = match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                details.insert("error".to_string(), err.to_string());
                return checks::error(
                    "daemon.exec",
                    "Could not build daemon exec probe HTTP client".to_string(),
                    None,
                    details,
                );
            }
        };
        let response = client
            .post(format!("{}/exec", local_url.trim_end_matches('/')))
            .json(&serde_json::json!({
                "runner_id": runner_id,
                "cwd": workspace_root,
                "command": ["homeboy", "--version"],
                "capture_patch": false
            }))
            .send();
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                details.insert("error".to_string(), err.to_string());
                return checks::error(
                    "daemon.exec",
                    "Connected runner daemon did not accept the exec probe".to_string(),
                    Some(format!(
                        "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}` before retrying Lab offload"
                    )),
                    details,
                );
            }
        };
        let status_code = response.status().as_u16();
        let body: serde_json::Value = match response.json() {
            Ok(body) => body,
            Err(err) => {
                details.insert("status".to_string(), status_code.to_string());
                details.insert("error".to_string(), err.to_string());
                return checks::error(
                    "daemon.exec",
                    "Connected runner daemon returned an invalid exec probe response".to_string(),
                    Some(format!(
                        "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}` before retrying Lab offload"
                    )),
                    details,
                );
            }
        };
        details.insert("status".to_string(), status_code.to_string());
        if let Some(job_id) = body
            .pointer("/data/body/job/id")
            .and_then(serde_json::Value::as_str)
        {
            details.insert("job_id".to_string(), job_id.to_string());
        }
        if status_code < 400
            && body.get("success").and_then(serde_json::Value::as_bool) == Some(true)
        {
            return checks::ok_with_details(
                "daemon.exec",
                "Connected runner daemon accepted a lightweight exec probe".to_string(),
                details,
            );
        }

        let error_payload = body
            .get("error")
            .or_else(|| body.get("data"))
            .unwrap_or(&body);
        details.insert("response".to_string(), error_payload.to_string());
        checks::error(
            "daemon.exec",
            "Connected runner daemon failed the lightweight exec probe".to_string(),
            Some(format!(
                "Reconnect runner `{runner_id}` with `homeboy runner connect {runner_id}` before retrying Lab offload"
            )),
            details,
        )
    }

    fn memory_from_proc_meminfo() -> Option<MemoryProbe> {
        let raw = fs::read_to_string("/proc/meminfo").ok()?;
        let total_kb = meminfo_value_kb(&raw, "MemTotal")?;
        let available_kb = meminfo_value_kb(&raw, "MemAvailable");
        Some(MemoryProbe {
            total_mb: total_kb / 1024,
            available_mb: available_kb.map(|kb| kb / 1024),
        })
    }

    fn memory_from_macos_sysctl() -> Option<MemoryProbe> {
        let total_bytes = common::local_command_line("sysctl", &["-n", "hw.memsize"])?;
        let total_mb = total_bytes.parse::<u64>().ok()? / 1024 / 1024;
        Some(MemoryProbe {
            total_mb,
            available_mb: None,
        })
    }

    fn meminfo_value_kb(raw: &str, key: &str) -> Option<u64> {
        raw.lines().find_map(|line| {
            let (name, rest) = line.split_once(':')?;
            if name != key {
                return None;
            }
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
    }

    fn browser_cache_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(path) = env::var("PLAYWRIGHT_BROWSERS_PATH") {
            if !path.trim().is_empty() {
                candidates.push(PathBuf::from(path));
            }
        }
        if let Ok(home) = env::var("HOME") {
            let home = PathBuf::from(home);
            candidates.push(home.join("Library").join("Caches").join("ms-playwright"));
            candidates.push(home.join(".cache").join("ms-playwright"));
        }
        candidates
    }
}

mod checks {
    use super::*;
    use types::{RunnerCheck, RunnerDoctorStatus, ToolProbe};

    pub fn tool_check(spec: probes::ToolSpec, probe: &ToolProbe) -> RunnerCheck {
        if probe.available {
            ok(
                spec.check_id,
                format!("{} is available", spec.command),
                None,
            )
        } else if spec.required {
            error(
                spec.check_id,
                format!("{} is required but was not found", spec.command),
                Some(spec.remediation.to_string()),
                BTreeMap::new(),
            )
        } else {
            warning(
                spec.check_id,
                format!("{} was not found", spec.command),
                Some(spec.remediation.to_string()),
            )
        }
    }

    pub fn required_tool_check(command: &str, probe: &ToolProbe) -> RunnerCheck {
        let mut details = BTreeMap::new();
        details.insert("command".to_string(), command.to_string());
        if let Some(path) = &probe.path {
            details.insert("path".to_string(), path.clone());
        }

        if probe.available {
            ok_with_details(
                format!("tool.required.{command}"),
                format!("Required runner tool {command} is available"),
                details,
            )
        } else {
            error(
                format!("tool.required.{command}"),
                format!("Required runner tool {command} was not found"),
                Some(format!(
                    "Install {command} on the runner and ensure it is on PATH, or remove it from the provider preflight requirements"
                )),
                details,
            )
        }
    }

    pub fn playwright_check(playwright: bool, browser_ready: bool) -> RunnerCheck {
        match (playwright, browser_ready) {
            (true, true) => ok(
                "playwright.browser_ready",
                "Playwright CLI and browser cache are detectable".to_string(),
                None,
            ),
            (true, false) => warning(
                "playwright.browser_ready",
                "Playwright CLI is available but browser readiness was not detected".to_string(),
                Some(
                    "Run `playwright install` in the relevant project if browser traces fail"
                        .to_string(),
                ),
            ),
            (false, true) => warning(
                "playwright.browser_ready",
                "Browser cache is present but Playwright CLI was not found".to_string(),
                Some("Install Playwright CLI in the runner environment".to_string()),
            ),
            (false, false) => warning(
                "playwright.browser_ready",
                "Playwright/browser readiness was not detected".to_string(),
                Some(
                    "Install Playwright and browser binaries for browser-backed traces".to_string(),
                ),
            ),
        }
    }

    pub fn path_writable_check(
        id: &'static str,
        writable: bool,
        path: &Path,
        remediation: &str,
    ) -> RunnerCheck {
        let mut details = BTreeMap::new();
        details.insert("path".to_string(), common::display_path(path));
        if writable {
            ok_with_details(
                id,
                "Path is writable by the runner user".to_string(),
                details,
            )
        } else {
            error(
                id,
                "Path is not writable by the runner user".to_string(),
                Some(remediation.to_string()),
                details,
            )
        }
    }

    pub fn homeboy_version_skew_check(
        local_version: &str,
        remote_version: &str,
        runner_id: &str,
        server_id: &str,
    ) -> Option<RunnerCheck> {
        let local_version = local_version.trim();
        let remote_version = remote_version.trim();
        if local_version.is_empty()
            || remote_version.is_empty()
            || remote_version == "unknown"
            || local_version == remote_version
        {
            return None;
        }

        let mut details = BTreeMap::new();
        details.insert("local_version".to_string(), local_version.to_string());
        details.insert("remote_version".to_string(), remote_version.to_string());
        Some(warning_with_details(
            "homeboy.version_skew",
            format!(
                "Local Homeboy {local_version} differs from remote runner Homeboy {remote_version}"
            ),
            Some(format!(
                "Upgrade Homeboy on runner `{runner_id}` to match the local client; for example: `homeboy ssh {server_id} -- homeboy upgrade --no-restart`, or rerun the runner setup/upgrade workflow"
            )),
            details,
        ))
    }

    pub fn ok(id: impl Into<String>, message: String, remediation: Option<String>) -> RunnerCheck {
        RunnerCheck {
            id: id.into(),
            status: RunnerDoctorStatus::Ok,
            message,
            remediation,
            details: BTreeMap::new(),
        }
    }

    pub fn warning(
        id: impl Into<String>,
        message: String,
        remediation: Option<String>,
    ) -> RunnerCheck {
        RunnerCheck {
            id: id.into(),
            status: RunnerDoctorStatus::Warning,
            message,
            remediation,
            details: BTreeMap::new(),
        }
    }

    fn warning_with_details(
        id: impl Into<String>,
        message: String,
        remediation: Option<String>,
        details: BTreeMap<String, String>,
    ) -> RunnerCheck {
        RunnerCheck {
            id: id.into(),
            status: RunnerDoctorStatus::Warning,
            message,
            remediation,
            details,
        }
    }

    pub fn error(
        id: impl Into<String>,
        message: String,
        remediation: Option<String>,
        details: BTreeMap<String, String>,
    ) -> RunnerCheck {
        RunnerCheck {
            id: id.into(),
            status: RunnerDoctorStatus::Error,
            message,
            remediation,
            details,
        }
    }

    pub fn overall_status(checks: &[RunnerCheck]) -> RunnerDoctorStatus {
        if checks
            .iter()
            .any(|check| check.status == RunnerDoctorStatus::Error)
        {
            RunnerDoctorStatus::Error
        } else if checks
            .iter()
            .any(|check| check.status == RunnerDoctorStatus::Warning)
        {
            RunnerDoctorStatus::Warning
        } else {
            RunnerDoctorStatus::Ok
        }
    }

    pub(super) fn ok_with_details(
        id: impl Into<String>,
        message: String,
        details: BTreeMap<String, String>,
    ) -> RunnerCheck {
        RunnerCheck {
            id: id.into(),
            status: RunnerDoctorStatus::Ok,
            message,
            remediation: None,
            details,
        }
    }
}

mod common {
    use super::*;

    pub fn local_command_line(command: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(command).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        first_output_line(&output.stdout, &output.stderr)
    }

    pub fn remote_line(client: &SshClient, command: &str) -> Option<String> {
        let output = client.execute(command);
        if !output.success {
            return None;
        }
        output
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    }

    pub fn first_output_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
        let combined = if stdout.is_empty() { stderr } else { stdout };
        String::from_utf8_lossy(combined)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string)
    }

    pub fn display_path(path: impl AsRef<Path>) -> String {
        path.as_ref().to_string_lossy().to_string()
    }

    pub fn shell_word(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    pub fn detail_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }
}

fn runner_summary(
    target_type: &'static str,
    runner: Option<&Runner>,
    server: Option<&Server>,
) -> types::RunnerTargetSummary {
    types::RunnerTargetSummary {
        target_type,
        registry: runner.map(|runner| types::RunnerRegistrySummary {
            id: runner.id.clone(),
            kind: runner.kind.clone(),
        }),
        server: server.map(|server| types::RunnerServerSummary {
            id: server.id.clone(),
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            is_localhost: matches!(server.host.as_str(), "localhost" | "127.0.0.1" | "::1"),
        }),
    }
}

fn normalized_extension_ids(extension_ids: &[String]) -> Vec<String> {
    let mut ids = extension_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn normalized_required_tools(commands: &[String]) -> Vec<String> {
    let mut tools = commands
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();
    tools
}

#[cfg(test)]
#[path = "doctor/tests.rs"]
mod tests;
