//! Runner-side rig source installation and removal.
//!
//! Cohesive group extracted from the rig materialization root: building the
//! runner-side `rig sources`/`rig install` dispatches, parsing installed source
//! metadata, and validating installed source paths. Kept in a sibling module so
//! the root stays under the structural item-count threshold (#5241).

use std::collections::HashMap;
use std::path::Path;

use crate::core::{Error, Result};

use super::super::{
    exec, RunnerCapabilityPreflight, RunnerExecOptions, RunnerExecOutput, RunnerRequiredTool,
};

/// Capability-parity contract for the runner-side `rig install` dispatch.
///
/// The install command is executed by the runner's `homeboy` binary, so the
/// runner must expose the `homeboy` tool. `exec` short-circuits this preflight
/// for local runners and for SSH runners that already advertise the tool, so it
/// is behavior-preserving on a provisioned runner and fails loudly before a
/// remote run that would otherwise error mid-dispatch (#5285).
pub(super) fn rig_install_capability_preflight() -> RunnerCapabilityPreflight {
    RunnerCapabilityPreflight {
        command: "rig.install".to_string(),
        required_tools: vec![RunnerRequiredTool::Homeboy],
        required_commands: Vec::new(),
        required_components: Vec::new(),
        required_env: Vec::new(),
    }
}

pub(super) fn remote_package_path(
    source_root: &str,
    package_path: &str,
    remote_source_root: &str,
) -> String {
    let source_root = Path::new(source_root);
    let package_path = Path::new(package_path);
    match package_path.strip_prefix(source_root) {
        Ok(relative) if !relative.as_os_str().is_empty() => Path::new(remote_source_root)
            .join(relative)
            .to_string_lossy()
            .to_string(),
        _ => remote_source_root.to_string(),
    }
}

/// Run a runner-side `rig sources <args...>` command, returning its captured
/// output and exit code. Extracted so the list/remove dispatches in
/// `remove_runner_installed_rig_source` share one `RunnerExecOptions`
/// construction instead of duplicating the boilerplate (#5283).
fn exec_runner_rig_sources_command(
    runner_id: &str,
    command_path: &str,
    remote_cwd: &str,
    args: &[&str],
) -> Result<(RunnerExecOutput, i32)> {
    let mut command = vec![
        command_path.to_string(),
        "rig".to_string(),
        "sources".to_string(),
    ];
    command.extend(args.iter().map(|arg| arg.to_string()));
    exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )
}

pub(super) fn remove_runner_installed_rig_source(
    runner_id: &str,
    command_path: &str,
    remote_cwd: &str,
    rig_id: &str,
) -> Result<Option<String>> {
    let (list_output, list_exit_code) =
        exec_runner_rig_sources_command(runner_id, command_path, remote_cwd, &["list"])?;
    if list_exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "rig",
            format!(
                "runner dispatch could not inspect installed rig sources on runner `{runner_id}`"
            ),
            Some(rig_id.to_string()),
            Some(vec![list_output.stderr.trim().to_string()]),
        ));
    }

    let Some(selector) = installed_source_selector_for_rig(&list_output.stdout, rig_id)? else {
        return Ok(None);
    };

    let (remove_output, remove_exit_code) = exec_runner_rig_sources_command(
        runner_id,
        command_path,
        remote_cwd,
        &["remove", selector.as_str()],
    )?;
    if remove_exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "rig",
            format!("runner dispatch could not remove stale rig source for `{rig_id}` on runner `{runner_id}`"),
            Some(rig_id.to_string()),
            Some(vec![remove_output.stderr.trim().to_string()]),
        ));
    }

    Ok(Some(selector))
}

fn installed_source_selector_for_rig(stdout: &str, rig_id: &str) -> Result<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse runner rig sources list output".to_string()),
            Some(stdout.chars().take(200).collect()),
        )
    })?;
    let sources = value
        .get("data")
        .and_then(|data| data.get("report"))
        .and_then(|report| report.get("sources"))
        .and_then(|sources| sources.as_array())
        .cloned()
        .unwrap_or_default();

    for source in sources {
        let has_rig = source
            .get("rigs")
            .and_then(|rigs| rigs.as_array())
            .is_some_and(|rigs| {
                rigs.iter().any(|rig| {
                    rig.get("id")
                        .and_then(|id| id.as_str())
                        .is_some_and(|id| id == rig_id)
                })
            });
        if !has_rig {
            continue;
        }
        for key in ["package_id", "package_path", "source"] {
            if let Some(selector) = source.get(key).and_then(|value| value.as_str()) {
                if !selector.trim().is_empty() {
                    return Ok(Some(selector.to_string()));
                }
            }
        }
    }

    Ok(None)
}

pub(super) fn validate_installed_rig_source(
    rig_id: &str,
    source_root: &str,
    package_path: &str,
) -> Result<()> {
    if Path::new(source_root).is_dir() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "rig",
        format!(
            "runner dispatch cannot materialize rig `{rig_id}` because its installed source metadata points at a missing path"
        ),
        Some(source_root.to_string()),
        Some(vec![
            format!(
                "Repair the rig source metadata with: homeboy rig install {} --id {} --reinstall",
                shell_arg(package_path),
                shell_arg(rig_id)
            ),
            format!(
                "Or remove the stale source metadata with: homeboy rig sources remove {}",
                shell_arg(package_path)
            ),
            "Run `homeboy rig sources list` to inspect installed rig sources.".to_string(),
        ]),
    ))
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rig_install_capability_preflight_requires_homeboy_tool() {
        let preflight = rig_install_capability_preflight();
        assert_eq!(preflight.command, "rig.install");
        assert_eq!(preflight.required_tools, vec![RunnerRequiredTool::Homeboy]);
        assert!(!preflight.is_empty());
    }

    #[test]
    fn installed_source_selector_prefers_package_id_for_matching_rig() {
        let stdout = serde_json::json!({
            "success": true,
            "data": {
                "command": "rig.sources.list",
                "report": {
                    "sources": [
                        {
                            "source": "/runner/old-source",
                            "package_id": "old-source",
                            "package_path": "/runner/old-source",
                            "rigs": [{ "id": "target-rig" }]
                        }
                    ]
                }
            }
        })
        .to_string();

        assert_eq!(
            installed_source_selector_for_rig(&stdout, "target-rig").unwrap(),
            Some("old-source".to_string())
        );
    }

    #[test]
    fn installed_source_selector_ignores_unrelated_sources() {
        let stdout = serde_json::json!({
            "success": true,
            "data": {
                "report": {
                    "sources": [
                        {
                            "source": "/runner/old-source",
                            "package_id": "old-source",
                            "package_path": "/runner/old-source",
                            "rigs": [{ "id": "other-rig" }]
                        }
                    ]
                }
            }
        })
        .to_string();

        assert_eq!(
            installed_source_selector_for_rig(&stdout, "target-rig").unwrap(),
            None
        );
    }

    #[test]
    fn stale_installed_rig_source_metadata_suggests_reinstall() {
        let err = validate_installed_rig_source(
            "woocommerce-performance",
            "/missing/homeboy-rigs/woocommerce/woocommerce",
            "/Users/user/Developer/sample-rigs@issue-323-runtime-fresh/sample-plugin/sample-plugin",
        )
        .expect_err("missing source should fail");

        assert!(err.message.contains("installed source metadata"));
        assert_eq!(err.details["field"], "rig");
        let hints = err.details["tried"]
            .as_array()
            .expect("tried hints")
            .iter()
            .filter_map(|hint| hint.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(hints.contains("homeboy rig install"));
        assert!(hints.contains("--id woocommerce-performance --reinstall"));
        assert!(hints.contains("homeboy rig sources list"));
    }
}
