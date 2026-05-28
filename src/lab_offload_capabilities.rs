use std::path::{Path, PathBuf};

use homeboy::cli_surface::Commands;

pub(crate) fn lab_runner_capability_contract(
    command: &Commands,
    source_path: &Path,
) -> Option<homeboy::core::runner::LabRunnerCapabilityContract> {
    if !command.supports_lab_runner() {
        return None;
    }

    let command_label = match command {
        Commands::Bench(args) if args.is_run_command() => "bench",
        Commands::Refactor(args) if args.is_hot_resource_command() => "refactor",
        Commands::Audit(args) if args.changed_since.is_none() && !args.conventions => "audit",
        Commands::Lint(args) if args.is_full_workspace_run() => "lint",
        Commands::Test(args) if args.changed_since.is_none() => "test",
        Commands::Trace(_) => "trace",
        _ => return None,
    };

    let mut required_tools = Vec::new();

    if source_path.join("package.json").is_file() {
        push_node_package_tool(
            &mut required_tools,
            homeboy::core::runner::RunnerRequiredTool::Npm,
        );
    }

    if source_path.join("pnpm-lock.yaml").is_file() {
        push_node_package_tool(
            &mut required_tools,
            homeboy::core::runner::RunnerRequiredTool::Pnpm,
        );
    }

    if source_path.join("composer.json").is_file() {
        push_unique(
            &mut required_tools,
            homeboy::core::runner::RunnerRequiredTool::Php,
        );
        push_unique(
            &mut required_tools,
            homeboy::core::runner::RunnerRequiredTool::Composer,
        );
    }

    if has_docker_signal(source_path) {
        push_unique(
            &mut required_tools,
            homeboy::core::runner::RunnerRequiredTool::Docker,
        );
    }

    Some(homeboy::core::runner::LabRunnerCapabilityContract {
        command: command_label,
        required_tools,
        requires_playwright: matches!(command, Commands::Trace(_)),
    })
}

pub(crate) fn lab_offload_source_path(args: &[String]) -> homeboy::core::Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" {
            let value = iter.next().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "path",
                    "--path requires a value before Lab offload can sync the workspace",
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
    }

    std::env::current_dir().map_err(|err| {
        homeboy::core::Error::internal_io(err.to_string(), Some("read cwd".to_string()))
    })
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn push_node_package_tool(
    required_tools: &mut Vec<homeboy::core::runner::RunnerRequiredTool>,
    package_tool: homeboy::core::runner::RunnerRequiredTool,
) {
    push_unique(
        required_tools,
        homeboy::core::runner::RunnerRequiredTool::Node,
    );
    push_unique(required_tools, package_tool);
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn lab_runner_capability_contract_detects_workspace_tool_signals() {
        let dir = tempfile::tempdir().expect("temp project");
        std::fs::write(dir.path().join("package.json"), "{}").expect("package.json");
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: '9'\n")
            .expect("pnpm-lock");
        std::fs::write(dir.path().join("composer.json"), "{}").expect("composer.json");
        std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");
        let command = homeboy::cli_surface::Cli::try_parse_from(["homeboy", "lint"])
            .expect("parse")
            .command;

        let contract = lab_runner_capability_contract(&command, dir.path()).expect("contract");

        assert_eq!(contract.command, "lint");
        assert_eq!(
            contract.required_tools,
            vec![
                homeboy::core::runner::RunnerRequiredTool::Node,
                homeboy::core::runner::RunnerRequiredTool::Npm,
                homeboy::core::runner::RunnerRequiredTool::Pnpm,
                homeboy::core::runner::RunnerRequiredTool::Php,
                homeboy::core::runner::RunnerRequiredTool::Composer,
                homeboy::core::runner::RunnerRequiredTool::Docker,
            ]
        );
    }
}
