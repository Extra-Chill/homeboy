use super::RunnerRequiredTool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabOffloadCommandPrefix {
    pub(crate) argv: Vec<String>,
    pub(crate) required_tools: Vec<RunnerRequiredTool>,
}

pub(crate) fn lab_offload_command_prefix(homeboy_path: &str) -> LabOffloadCommandPrefix {
    // The runner already owns this execution. Keep nested agent-task and other
    // hot commands from applying controller resource policy and offloading again.
    let argv = vec![
        homeboy_path.to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "--wait".to_string(),
    ];
    let program = executable_name(homeboy_path);
    let required_tools = if program == "homeboy" {
        vec![RunnerRequiredTool::homeboy()]
    } else {
        vec![RunnerRequiredTool::new(program)]
    };

    LabOffloadCommandPrefix {
        argv,
        required_tools,
    }
}

fn executable_name(value: &str) -> &str {
    value.rsplit('/').next().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_runs_the_runner_homeboy_path_locally_and_requires_homeboy() {
        for homeboy_path in ["/usr/local/bin/homeboy", "homeboy"] {
            let prefix = lab_offload_command_prefix(homeboy_path);

            assert_eq!(
                prefix.argv,
                vec![homeboy_path, "--placement", "local", "--wait"]
            );
            assert_eq!(prefix.required_tools, vec![RunnerRequiredTool::homeboy()]);
        }
    }
}
