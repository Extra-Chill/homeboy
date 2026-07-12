use std::path::Path;

use super::{LabOffloadCommand, LabRunnerCapabilityContract, RunnerRequiredTool};

pub(super) fn lab_runner_capability_contract(
    command: &LabOffloadCommand,
    source_path: &Path,
    command_prefix_required_tools: &[RunnerRequiredTool],
) -> Option<LabRunnerCapabilityContract> {
    if !command.is_portable() {
        return None;
    }

    let mut required_tools = Vec::new();

    for tool in command_prefix_required_tools {
        push_unique(&mut required_tools, tool.clone());
    }

    let _ = source_path;

    Some(LabRunnerCapabilityContract {
        command: command.hot_label,
        required_tools,
        required_capabilities: command
            .required_capabilities
            .iter()
            .map(|capability| capability.name.clone())
            .collect(),
    })
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}
