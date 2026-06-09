use std::path::Path;

use super::{LabOffloadCommand, LabRunnerCapabilityContract, RunnerRequiredTool};

pub(super) fn lab_runner_capability_contract(
    command: &LabOffloadCommand,
    source_path: &Path,
    command_prefix_required_tools: &[RunnerRequiredTool],
) -> Option<LabRunnerCapabilityContract> {
    if !command.portable {
        return None;
    }

    let mut required_tools = Vec::new();

    for tool in command_prefix_required_tools {
        push_unique(&mut required_tools, *tool);
    }

    if command.infer_source_path_tools {
        if source_path.join(concat!("package", ".json")).is_file() {
            push_node_package_tool(&mut required_tools, RunnerRequiredTool::Npm);
        }

        if source_path.join("pnpm-lock.yaml").is_file() {
            push_node_package_tool(&mut required_tools, RunnerRequiredTool::Pnpm);
        }

        if source_path.join(concat!("com", "poser", ".json")).is_file() {
            push_unique(&mut required_tools, RunnerRequiredTool::Php);
            push_unique(&mut required_tools, RunnerRequiredTool::Composer);
        }

        if has_docker_signal(source_path) {
            push_unique(&mut required_tools, RunnerRequiredTool::Docker);
        }
    }

    Some(LabRunnerCapabilityContract {
        command: command.hot_label,
        required_tools,
        requires_playwright: command.requires_playwright,
    })
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn push_node_package_tool(
    required_tools: &mut Vec<RunnerRequiredTool>,
    package_tool: RunnerRequiredTool,
) {
    push_unique(required_tools, RunnerRequiredTool::Node);
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
