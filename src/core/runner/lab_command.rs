use std::path::Path;

use crate::core::component;

use super::{required_tool_for_command_name, RunnerRequiredTool};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabOffloadCommandPrefix {
    pub(crate) argv: Vec<String>,
    pub(crate) required_tools: Vec<RunnerRequiredTool>,
}

pub(crate) fn lab_offload_command_prefix(
    source_path: &Path,
    homeboy_path: &str,
) -> LabOffloadCommandPrefix {
    let configured_prefix = component::discover_from_portable(source_path)
        .and_then(|component| component.lab)
        .map(|lab| lab.self_command_prefix)
        .filter(|prefix| !prefix.is_empty());

    let argv = configured_prefix.unwrap_or_else(|| vec![homeboy_path.to_string()]);
    let required_tools = required_tools_for_command_prefix(&argv);

    LabOffloadCommandPrefix {
        argv,
        required_tools,
    }
}

fn required_tools_for_command_prefix(argv: &[String]) -> Vec<RunnerRequiredTool> {
    let Some(program) = argv.first().map(|value| executable_name(value)) else {
        return Vec::new();
    };

    required_tool_for_command_name(program)
        .into_iter()
        .collect()
}

fn executable_name(value: &str) -> &str {
    value.rsplit('/').next().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_self_command_prefix_overrides_runner_homeboy_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{"id":"project","lab":{"self_command_prefix":["cargo","run","--quiet","--bin","homeboy","--"]}}"#,
        )
        .expect("write homeboy.json");

        let prefix = lab_offload_command_prefix(dir.path(), "/usr/local/bin/homeboy");

        assert_eq!(
            prefix.argv,
            vec![
                "cargo".to_string(),
                "run".to_string(),
                "--quiet".to_string(),
                "--bin".to_string(),
                "homeboy".to_string(),
                "--".to_string(),
            ]
        );
        assert_eq!(prefix.required_tools, vec![RunnerRequiredTool::Cargo]);
    }

    #[test]
    fn missing_self_command_prefix_uses_runner_homeboy_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("homeboy.json"), r#"{"id":"project"}"#)
            .expect("write homeboy.json");

        let prefix = lab_offload_command_prefix(dir.path(), "/usr/local/bin/homeboy");

        assert_eq!(prefix.argv, vec!["/usr/local/bin/homeboy".to_string()]);
        assert_eq!(prefix.required_tools, vec![RunnerRequiredTool::Homeboy]);
    }

    #[test]
    fn bare_workspace_uses_runner_homeboy_path() {
        let dir = tempfile::tempdir().expect("temp dir");

        let prefix = lab_offload_command_prefix(dir.path(), "homeboy");

        assert_eq!(prefix.argv, vec!["homeboy".to_string()]);
        assert_eq!(prefix.required_tools, vec![RunnerRequiredTool::Homeboy]);
    }
}
