//! Shell command pipeline steps (`command`, `command-if-missing`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use super::super::expand::expand_vars;
use super::super::spec::{PipelineStep, RigSpec};
use super::super::toolchain;
use crate::core::error::{Error, Result};

pub(super) fn run_command_step(
    rig: &RigSpec,
    cmd: &str,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    settings: &[(String, String)],
) -> Result<()> {
    let expanded = expand_vars(rig, cmd);
    let mut command = Command::new(command_step_shell());
    command.arg("-c").arg(&expanded);

    if let Some(cwd) = cwd {
        let resolved = expand_vars(rig, cwd);
        command.current_dir(PathBuf::from(resolved));
    }

    if !env.contains_key("PATH") {
        if let Some(path) = toolchain::command_step_path() {
            command.env("PATH", path);
        }
    }

    for (k, v) in env {
        command.env(k, expand_vars(rig, v));
    }

    for (k, v) in settings_env(settings) {
        command.env(k, v);
    }

    let status = command.status().map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("spawn failed for `{}`: {}", expanded, e),
        )
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let hint = if code == 127 {
            ": command not found. Rig command steps bootstrap common toolchain PATHs automatically; if the tool lives elsewhere, set env.PATH for this step or prefer a typed build/git/check step"
        } else {
            ""
        };
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("`{}` exited {}{}", expanded, code, hint),
        ));
    }
    Ok(())
}

fn settings_env(settings: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for (key, value) in settings {
        env.push((format!("HOMEBOY_SETTINGS_{}", key.to_uppercase()), value.clone()));
        let sanitized = shell_safe_setting_env_key(key);
        let raw = format!("HOMEBOY_SETTINGS_{}", key.to_uppercase());
        if sanitized != raw {
            env.push((sanitized, value.clone()));
        }
    }
    env
}

fn shell_safe_setting_env_key(key: &str) -> String {
    let normalized = key
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("HOMEBOY_SETTINGS_{normalized}")
}

pub(super) fn run_command_pipeline_step(
    rig: &RigSpec,
    step: &PipelineStep,
    settings: &[(String, String)],
) -> Result<()> {
    match step {
        PipelineStep::Command { cmd, cwd, env, .. } => {
            run_command_step(rig, cmd, cwd.as_deref(), env, settings)
        }
        PipelineStep::CommandIfMissing {
            missing,
            cmd,
            cwd,
            env,
            ..
        } => run_command_if_missing_step(rig, missing, cmd, cwd.as_deref(), env, settings),
        _ => unreachable!("command pipeline helper only accepts command steps"),
    }
}

fn run_command_if_missing_step(
    rig: &RigSpec,
    missing: &str,
    cmd: &str,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    settings: &[(String, String)],
) -> Result<()> {
    let expanded_missing = expand_vars(rig, missing);
    let missing_path = PathBuf::from(&expanded_missing);
    let resolved_missing = if missing_path.is_absolute() {
        missing_path
    } else if let Some(cwd) = cwd {
        PathBuf::from(expand_vars(rig, cwd)).join(missing_path)
    } else {
        missing_path
    };

    if resolved_missing.exists() {
        return Ok(());
    }

    run_command_step(rig, cmd, cwd, env, settings)
}

#[cfg(unix)]
fn command_step_shell() -> &'static str {
    "/bin/sh"
}

#[cfg(not(unix))]
fn command_step_shell() -> &'static str {
    "sh"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_env_adds_shell_safe_dotted_keys() {
        let env = settings_env(&[(
            "components.woocommerce.extensions.wordpress.wp_codebox_source_root".to_string(),
            "/workspace/source".to_string(),
        )]);

        assert!(env.iter().any(|(key, value)| {
            key == "HOMEBOY_SETTINGS_COMPONENTS_WOOCOMMERCE_EXTENSIONS_WORDPRESS_WP_CODEBOX_SOURCE_ROOT"
                && value == "/workspace/source"
        }));
    }
}
