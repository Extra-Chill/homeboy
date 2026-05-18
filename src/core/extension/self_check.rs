use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionCapability;
use crate::core::server::{
    execute_local_command_in_dir, execute_local_command_passthrough, CommandOutput,
};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SelfCheckOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_self_checks(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
) -> Result<SelfCheckOutput> {
    run_self_checks_with_passthrough(component, capability, source_path, true)
}

pub(crate) fn run_self_checks_with_passthrough(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
) -> Result<SelfCheckOutput> {
    let commands = component.script_commands(capability);
    if commands.is_empty() {
        return Err(Error::validation_invalid_argument(
            "self_checks",
            format!(
                "Component '{}' has no {} self-check commands configured",
                component.id,
                capability.label()
            ),
            None,
            None,
        ));
    }

    let working_dir = source_path.to_string_lossy();
    let mut stdout = String::new();
    let mut stderr = String::new();

    for command in commands {
        if passthrough {
            crate::log_status!(
                "self-check",
                "running {} self-check for {}: {}",
                capability.label(),
                component.id,
                command
            );
        }
        let output = execute_self_check_command(command, &working_dir, passthrough);
        stdout.push_str(&output.stdout);
        stderr.push_str(&output.stderr);

        if !output.success {
            return Ok(SelfCheckOutput {
                exit_code: output.exit_code,
                success: false,
                stdout,
                stderr,
            });
        }
    }

    Ok(SelfCheckOutput {
        exit_code: 0,
        success: true,
        stdout,
        stderr,
    })
}

fn execute_self_check_command(
    command: &str,
    working_dir: &str,
    passthrough: bool,
) -> CommandOutput {
    if passthrough {
        execute_local_command_passthrough(command, Some(working_dir), None)
    } else {
        execute_local_command_in_dir(command, Some(working_dir), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::{Component, SelfCheckConfig};

    #[test]
    fn test_run_self_checks_requires_configured_commands() {
        let component = Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            "".to_string(),
            None,
        );

        let err = run_self_checks(&component, ExtensionCapability::Test, Path::new("/tmp"))
            .expect_err("missing self-checks should fail");

        assert!(err.to_string().contains("no test self-check commands"));
    }

    #[test]
    fn test_run_self_checks_runs_commands_in_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("one.sh"), "printf one >> order.txt\n")
            .expect("first script should be written");
        std::fs::write(dir.path().join("two.sh"), "printf two >> order.txt\n")
            .expect("second script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.self_checks = Some(SelfCheckConfig {
            lint: vec!["sh one.sh".to_string(), "sh two.sh".to_string()],
            test: Vec::new(),
        });

        let output = run_self_checks(&component, ExtensionCapability::Lint, dir.path())
            .expect("self-checks should run");

        assert!(output.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("order.txt")).unwrap(),
            "onetwo"
        );
    }

    #[test]
    fn test_run_self_checks_with_passthrough() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("lint.sh"),
            "printf self-check-stdout\nprintf self-check-stderr >&2\n",
        )
        .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.self_checks = Some(SelfCheckConfig {
            lint: vec!["sh lint.sh".to_string()],
            test: Vec::new(),
        });

        let output = run_self_checks_with_passthrough(
            &component,
            ExtensionCapability::Lint,
            dir.path(),
            false,
        )
        .expect("self-checks should run without terminal passthrough");

        assert!(output.success);
        assert_eq!(output.stdout, "self-check-stdout");
        assert_eq!(output.stderr, "self-check-stderr");
    }
}
