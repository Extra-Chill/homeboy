use std::path::Path;

use super::RunnerRequiredTool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabOffloadCommandPrefix {
    pub(crate) argv: Vec<String>,
    pub(crate) required_tools: Vec<RunnerRequiredTool>,
}

pub(crate) fn lab_offload_command_prefix(
    _source_path: &Path,
    homeboy_path: &str,
) -> LabOffloadCommandPrefix {
    let argv = vec![homeboy_path.to_string()];
    let required_tools = required_tools_for_command_prefix(&argv);

    LabOffloadCommandPrefix {
        argv,
        required_tools,
    }
}

fn required_tools_for_command_prefix(argv: &[String]) -> Vec<RunnerRequiredTool> {
    let Some(program) = argv.first().map(|value| executable_name(value)) else {
        return vec![RunnerRequiredTool::Homeboy];
    };

    match program {
        "cargo" => vec![RunnerRequiredTool::Cargo],
        "homeboy" => vec![RunnerRequiredTool::Homeboy],
        _ => Vec::new(),
    }
}

fn executable_name(value: &str) -> &str {
    value.rsplit('/').next().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_legacy_self_command_prefix() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{"id":"project","lab":{"self_command_prefix":["cargo","run","--quiet","--bin","homeboy","--"]}}"#,
        )
        .expect("write homeboy.json");

        let prefix = lab_offload_command_prefix(dir.path(), "/usr/local/bin/homeboy");

        assert_eq!(prefix.argv, vec!["/usr/local/bin/homeboy".to_string()]);
        assert_eq!(prefix.required_tools, vec![RunnerRequiredTool::Homeboy]);
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
