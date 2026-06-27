use super::*;
use crate::core::build_identity;
use crate::core::runner::{
    Runner, RunnerActiveJobState, RunnerExecMode, RunnerExecOutput, RunnerKind, RunnerRequiredTool,
    RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport,
};
use crate::core::server::RunnerSettings;
use crate::core::upgrade::helpers::current_version;
use crate::core::upgrade::types::InstallMethod;
use crate::core::upgrade::ExtensionUpgradeEntry;
use crate::core::Result;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

thread_local! {
    static LOCAL_VERSION_OVERRIDE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Returns the thread-local local-version override, if one is active.
pub(super) fn local_version_override() -> Option<String> {
    LOCAL_VERSION_OVERRIDE.with(|cell| cell.borrow().clone())
}

/// RAII guard that pins the local/current homeboy version for the current
/// thread, restoring the previous value on drop. Used by runner-upgrade
/// tests so their hardcoded fixture versions stay comparable against a
/// stable "local" version regardless of the live crate version.
struct LocalVersionGuard {
    previous: Option<String>,
}

impl LocalVersionGuard {
    fn set(version: &str) -> Self {
        let previous =
            LOCAL_VERSION_OVERRIDE.with(|cell| cell.borrow_mut().replace(version.to_string()));
        Self { previous }
    }
}

impl Drop for LocalVersionGuard {
    fn drop(&mut self) {
        LOCAL_VERSION_OVERRIDE.with(|cell| *cell.borrow_mut() = self.previous.take());
    }
}

/// Pins the local version low enough that no fixture runner version below the
/// live crate version is treated as drift. Tests that mock a runner reporting
/// versions like `0.228.x` use this so the version-drift guard added in #5566
/// does not spuriously skip them once the crate version climbs higher.
fn pin_local_version_for_fixtures() -> LocalVersionGuard {
    LocalVersionGuard::set("0.0.0")
}

#[test]
fn upgrades_configured_runner_with_homeboy_path_and_skip_runner_guard() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.local/bin/homeboy"));
    let mut commands = Vec::new();
    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push((
                runner_id.to_string(),
                options.command.clone(),
                options.allow_diagnostic_ssh,
            ));
            let stdout = match commands.len() {
                1 => "homeboy 0.199.1\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.199.2\n",
                4 => "homeboy 0.199.2\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].runner_id, "lab");
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].previous_version.as_deref(), Some("0.199.1"));
    assert_eq!(updated[0].new_version.as_deref(), Some("0.199.2"));
    assert_eq!(
        commands[1].1,
        vec![
            "/home/user/.local/bin/homeboy",
            "upgrade",
            "--no-restart",
            "--skip-extensions",
            "--skip-runners"
        ]
    );
    assert!(commands.iter().all(|(_, _, allow_ssh)| *allow_ssh));

    let capability_plan = runner_upgrade_capability_plan();
    assert_eq!(
        capability_plan.required_tools,
        vec![RunnerRequiredTool::Homeboy]
    );
    assert_eq!(capability_plan.command, "homeboy upgrade");
}

#[test]
fn materializes_forced_source_upgrade_path_before_forwarding_to_runner() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    let source_path =
        Path::new("/Users/user/Developer/homeboy@fix-bench-selected-duplicate-validation-1266");
    let mut calls = Vec::new();
    let mut materialized = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_and_source_materializer(
        &[runner],
        true,
        Some(InstallMethod::Source),
        Some(source_path),
        &[],
        |runner_id, options| {
            calls.push((options.command.clone(), options.cwd.clone()));
            let stdout = match calls.len() {
                1 => "homeboy 0.228.13\n",
                2 => "prepared source checkout\n",
                3 => "{\"install_method\":\"source\",\"message\":\"Upgrade command completed but active binary is still 0.228.13\",\"upgraded\":false}\n",
                4 => "homeboy 0.228.13\n",
                5 => "homeboy 0.228.13\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
        |runner, path| {
            materialized.push((runner.id.clone(), path.display().to_string()));
            Ok("/home/user/Developer/_lab_workspaces/homeboy-source".to_string())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(!updated[0].upgraded);
    assert_eq!(
        materialized,
        vec![(
            "lab".to_string(),
            "/Users/user/Developer/homeboy@fix-bench-selected-duplicate-validation-1266"
                .to_string()
        )]
    );
    assert_eq!(&calls[1].0[..2], &["sh".to_string(), "-lc".to_string()]);
    assert_eq!(
        calls[1].1.as_deref(),
        Some("/home/user/Developer/_lab_workspaces/homeboy-source")
    );
    assert!(calls[1].0[2].contains("git fetch origin"));
    assert!(calls[1].0[2].contains("git checkout --detach"));
    assert_eq!(
        calls[2].0,
        vec![
            "/home/user/.cargo/bin/homeboy",
            "upgrade",
            "--no-restart",
            "--skip-extensions",
            "--skip-runners",
            "--force",
            "--method",
            "source",
            "--source-path",
            "/home/user/Developer/_lab_workspaces/homeboy-source",
        ]
    );
}

#[test]
fn reports_runner_upgrade_failure_without_stopping_other_runners() {
    let _local_version = pin_local_version_for_fixtures();
    let runners = vec![ssh_runner("lab", None), ssh_runner("bench", None)];
    let mut calls = HashMap::<String, usize>::new();
    let (updated, skipped) = upgrade_runners_with_executor(
        &runners,
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            let count = calls.entry(runner_id.to_string()).or_default();
            *count += 1;
            match (runner_id, *count) {
                ("lab", 1) => Ok((
                    exec_output(runner_id, options.command, "homeboy 0.199.1\n", "", 0),
                    0,
                )),
                ("lab", 2) => Ok((
                    exec_output(runner_id, options.command, "", "download failed", 1),
                    1,
                )),
                ("bench", 1) => Ok((
                    exec_output(runner_id, options.command, "homeboy 0.199.1\n", "", 0),
                    0,
                )),
                ("bench", 2) => Ok((
                    exec_output(runner_id, options.command, "already latest", "", 0),
                    0,
                )),
                ("bench", 3) => Ok((
                    exec_output(runner_id, options.command, "homeboy 0.199.1\n", "", 0),
                    0,
                )),
                _ => panic!("unexpected call {runner_id} {count}"),
            }
        },
        runner_status,
    );

    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].runner_id, "bench");
    assert!(!updated[0].upgraded);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].runner_id, "lab");
    assert!(!skipped[0].success);
    assert_eq!(skipped[0].exit_code, 1);
    assert!(skipped[0].detail.contains("download failed"));
}

#[test]
fn syncs_extension_revisions_after_runner_upgrade() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    let extension_updates = vec![ExtensionUpgradeEntry {
        extension_id: "wordpress".to_string(),
        old_version: "2.116.4".to_string(),
        new_version: "2.117.2".to_string(),
        linked: true,
        source_path: Some("/Users/user/Developer/homeboy-extensions/wordpress".to_string()),
        git_root: Some("/Users/user/Developer/homeboy-extensions".to_string()),
        source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
        source_revision: Some("48517ac3".to_string()),
        source_update: Default::default(),
    }];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.228.4\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.228.5\n",
                4 => "{\"success\":true}\n",
                5 => "{\"success\":true}\n",
                6 => "homeboy 0.228.5\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert_eq!(
        commands[4],
        vec![
            "/home/user/.cargo/bin/homeboy",
            "extension",
            "install",
            "https://github.com/Extra-Chill/homeboy-extensions.git",
            "--id",
            "wordpress",
            "--ref",
            "48517ac3",
            "--replace",
        ]
    );
}

#[test]
fn installs_missing_runner_extension_without_replace_flag() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    let extension_updates = vec![ExtensionUpgradeEntry {
        extension_id: "swift".to_string(),
        old_version: "2.6.1".to_string(),
        new_version: "2.6.1".to_string(),
        linked: true,
        source_path: Some("/Users/user/Developer/homeboy-extensions/swift".to_string()),
        git_root: Some("/Users/user/Developer/homeboy-extensions".to_string()),
        source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
        source_revision: Some("98a61eda".to_string()),
        source_update: Default::default(),
    }];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, exit_code) = match commands.len() {
                1 => ("homeboy 0.228.4\n", 0),
                2 => ("{\"success\":true}\n", 0),
                3 => ("homeboy 0.228.5\n", 0),
                4 => (
                    "{\"success\":false,\"error\":{\"code\":\"extension.not_found\"}}\n",
                    4,
                ),
                5 => ("{\"success\":true}\n", 0),
                6 => ("homeboy 0.228.5\n", 0),
                _ => ("", 0),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, "", exit_code),
                exit_code,
            ))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert_eq!(
        commands[4],
        vec![
            "/home/user/.cargo/bin/homeboy",
            "extension",
            "install",
            "https://github.com/Extra-Chill/homeboy-extensions.git",
            "--id",
            "swift",
            "--ref",
            "98a61eda",
        ]
    );
}

#[test]
fn isolates_runner_extension_sync_failures_and_continues_later_extensions() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    let extension_updates = vec![
        extension_update("swift", "98a61eda"),
        extension_update("wordpress", "48517ac3"),
    ];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => ("homeboy 0.228.4\n", "", 0),
                2 => ("{\"success\":true}\n", "", 0),
                3 => ("homeboy 0.228.5\n", "", 0),
                4 => ("{\"success\":true}\n", "", 0),
                5 => ("", "swift failed", 1),
                6 => ("{\"success\":true}\n", "", 0),
                7 => ("{\"success\":true}\n", "", 0),
                8 => ("homeboy 0.228.5\n", "", 0),
                _ => ("", "", 0),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].extensions_failed.len(), 1);
    assert_eq!(skipped[0].extensions_failed[0].extension_id, "swift");
    assert_eq!(
        skipped[0].extensions_failed[0].recovery_commands,
        vec!["homeboy runner exec lab --ssh -- /home/user/.cargo/bin/homeboy extension install https://github.com/Extra-Chill/homeboy-extensions.git --id swift --ref 98a61eda --replace"]
    );
    assert_eq!(skipped[0].extensions_synced.len(), 1);
    assert_eq!(skipped[0].extensions_synced[0].extension_id, "wordpress");
    assert!(skipped[0].detail.contains("swift@98a61eda"));
    assert!(skipped[0]
        .detail
        .contains("homeboy upgrade --force --upgrade-runner lab"));
    assert!(commands
        .iter()
        .any(|command| command.contains(&"wordpress".to_string())));
}

#[test]
fn skips_runner_extensions_outside_supported_extension_policy() {
    let _local_version = pin_local_version_for_fixtures();
    let mut runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    runner.policy.supported_extensions = vec!["wordpress".to_string()];
    let extension_updates = vec![
        extension_update("swift", "98a61eda"),
        extension_update("wordpress", "48517ac3"),
    ];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            assert!(
                !options.command.contains(&"swift".to_string()),
                "unsupported Swift extension should not be synced"
            );
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.228.4\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.228.5\n",
                4 => "{\"success\":true}\n",
                5 => "{\"success\":true}\n",
                6 => "homeboy 0.228.5\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert_eq!(updated[0].extensions_synced.len(), 1);
    assert_eq!(updated[0].extensions_synced[0].extension_id, "wordpress");
    assert_eq!(updated[0].extensions_skipped.len(), 1);
    assert_eq!(updated[0].extensions_skipped[0].extension_id, "swift");
    assert_eq!(updated[0].extensions_failed.len(), 0);
    assert!(updated[0].extensions_skipped[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("supported_extensions"));
    assert!(updated[0]
        .detail
        .contains("runner extension sync(s) skipped"));
    assert!(commands
        .iter()
        .any(|command| command.contains(&"wordpress".to_string())));
}

#[test]
fn defers_extension_failures_when_runner_refresh_leaves_path_drift() {
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    let extension_updates = vec![
        extension_update("auxiliary-extension", "98a61eda"),
        extension_update("required-extension", "48517ac3"),
    ];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        true,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => ("homeboy 0.228.18\n", "", 0),
                2 => (
                    "{\"success\":true,\"message\":\"Upgrade command completed but active binary is still 0.228.18\"}\n",
                    "",
                    0,
                ),
                3 => ("homeboy 0.228.18\n", "", 0),
                4 => ("{\"success\":true}\n", "", 0),
                5 => ("", "extension setup failed", 1),
                6 => ("{\"success\":true}\n", "", 0),
                7 => ("{\"success\":true}\n", "", 0),
                8 => ("homeboy 0.228.13\n", "", 0),
                _ => ("", "unexpected runner command", 1),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(!skipped[0].success);
    assert!(!skipped[0].upgraded);
    assert_eq!(skipped[0].previous_version.as_deref(), Some("0.228.18"));
    assert_eq!(skipped[0].new_version.as_deref(), Some("0.228.18"));
    assert!(skipped[0].path_drift.is_some());
    assert!(skipped[0].extensions_failed.is_empty());
    assert_eq!(skipped[0].extensions_synced.len(), 1);
    assert_eq!(
        skipped[0].extensions_synced[0].extension_id,
        "required-extension"
    );
    assert_eq!(skipped[0].extensions_skipped.len(), 1);
    assert_eq!(
        skipped[0].extensions_skipped[0].extension_id,
        "auxiliary-extension"
    );
    let skipped_detail = skipped[0].extensions_skipped[0].detail.as_deref().unwrap();
    assert!(skipped_detail.contains("deferred because runner executable drift"));
    assert!(skipped_detail.contains("extension setup failed"));
    assert!(skipped[0]
        .detail
        .contains("runner extension sync(s) skipped"));
    assert!(!skipped[0]
        .detail
        .contains("runner extension sync(s) failed"));
    assert!(commands
        .iter()
        .any(|command| command.contains(&"required-extension".to_string())));
}

#[test]
fn upgrades_runner_binary_before_controller_scoped_extension_sync() {
    let _local_version = pin_local_version_for_fixtures();
    let mut runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy"));
    runner.policy.supported_extensions = vec!["required-extension".to_string()];
    let extension_updates = vec![
        extension_update("irrelevant-extension", "98a61eda"),
        extension_update("required-extension", "48517ac3"),
    ];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => ("homeboy 0.228.18\n", "", 0),
                2 => {
                    assert!(
                        options.command.contains(&"--skip-extensions".to_string()),
                        "runner binary upgrade must not run stale runner-side extension sync"
                    );
                    ("{\"success\":true}\n", "", 0)
                }
                3 => ("homeboy 0.228.21\n", "", 0),
                4 => ("{\"success\":true}\n", "", 0),
                5 => ("{\"success\":true}\n", "", 0),
                6 => ("homeboy 0.228.21\n", "", 0),
                _ => ("", "unexpected runner command", 1),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert_eq!(updated[0].previous_version.as_deref(), Some("0.228.18"));
    assert_eq!(updated[0].new_version.as_deref(), Some("0.228.21"));
    assert_eq!(updated[0].extensions_synced.len(), 1);
    assert_eq!(
        updated[0].extensions_synced[0].extension_id,
        "required-extension"
    );
    assert_eq!(updated[0].extensions_skipped.len(), 1);
    assert_eq!(
        updated[0].extensions_skipped[0].extension_id,
        "irrelevant-extension"
    );
    assert!(commands
        .iter()
        .all(|command| !command.contains(&"irrelevant-extension".to_string())));
    assert!(commands
        .iter()
        .any(|command| command.contains(&"required-extension".to_string())));
}

#[test]
fn repairs_stale_bare_homeboy_after_configured_runner_upgrade() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner(
        "lab",
        Some("/home/user/Developer/_lab_workspaces/homeboy-current/target/release/homeboy"),
    );
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.228.4\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.228.5\n",
                4 => "homeboy 0.228.4\n",
                5 => "{\"success\":true}\n",
                6 => "homeboy 0.228.5\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.228.5"));
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(commands[4][0], "homeboy");
    assert_eq!(commands[4][1], "upgrade");
    assert!(commands[4].contains(&"--skip-runners".to_string()));
    assert!(updated[0]
        .detail
        .contains("PATH-visible `homeboy` repaired"));
    assert!(!updated[0].detail.contains("runner PATH drift detected"));
}

#[test]
fn fails_when_configured_runner_path_remains_older_than_local_after_successful_upgrade() {
    let runner = ssh_runner("homeboy-lab", Some("/home/user/.cargo/bin/homeboy"));
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        true,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.0.1\n",
                2 => "{\"success\":true,\"message\":\"upgrade completed\"}\n",
                3 => "homeboy 0.0.1\n",
                4 => "homeboy 0.0.1\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(!skipped[0].success);
    assert_eq!(skipped[0].previous_version.as_deref(), Some("0.0.1"));
    assert_eq!(skipped[0].new_version.as_deref(), Some("0.0.1"));
    assert!(skipped[0]
        .path_drift
        .as_deref()
        .unwrap()
        .contains("but local/current reports"));
    assert!(skipped[0]
        .recovery_commands
        .contains(&"homeboy runner exec homeboy-lab -- homeboy upgrade --no-restart".to_string()));
    assert!(skipped[0]
        .detail
        .contains("runner version check: local/current"));
    assert!(skipped[0].detail.contains("runner before 0.0.1"));
    assert!(skipped[0].detail.contains("runner after 0.0.1"));
    assert!(skipped[0]
        .detail
        .contains("homeboy runner exec homeboy-lab -- homeboy upgrade --no-restart"));
    assert_eq!(commands[1][0], "/home/user/.cargo/bin/homeboy");
    assert!(commands[1].contains(&"--force".to_string()));
}

#[test]
fn fails_when_managed_bare_homeboy_repair_leaves_path_drift() {
    let runner = ssh_runner(
        "lab",
        Some("/home/user/Developer/_lab_workspaces/homeboy-current/target/release/homeboy"),
    );
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.228.4\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.228.5\n",
                4 => "homeboy 0.228.4\n",
                5 => "{\"success\":true}\n",
                6 => "homeboy 0.228.4\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].bare_homeboy_version.as_deref(), Some("0.228.4"));
    assert!(skipped[0]
        .path_drift
        .as_deref()
        .unwrap()
        .contains("managed PATH-visible `homeboy` repair left bare `homeboy` at 0.228.4"));
    assert!(skipped[0]
        .recovery_commands
        .contains(&"homeboy upgrade --force --upgrade-runner lab".to_string()));
    assert!(skipped[0]
        .detail
        .contains("managed PATH-visible `homeboy` repair completed"));
    assert!(skipped[0].detail.contains("runner PATH drift detected"));
}

#[test]
fn updates_versioned_runner_homeboy_path_to_bare_homeboy_when_newer() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy-0.229.1"));
    let extension_updates = vec![extension_update("required-extension", "48517ac3")];
    let mut commands = Vec::new();
    let mut path_updates = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
        &[runner],
        false,
        None,
        None,
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.229.1\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.229.1\n",
                4 => "homeboy 0.229.3\n",
                5 => "{\"success\":true}\n",
                6 => "{\"success\":true}\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
        |_runner, _path| unreachable!("source materialization not used"),
        |runner_id, homeboy_path| {
            path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
            Ok(())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].homeboy_path, "homeboy");
    assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.1"));
    assert_eq!(updated[0].new_version.as_deref(), Some("0.229.3"));
    assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.229.3"));
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(
        path_updates,
        vec![("lab".to_string(), "homeboy".to_string())]
    );
    assert_eq!(
        commands[4],
        vec!["homeboy", "extension", "show", "required-extension"]
    );
    assert!(updated[0].detail.contains(
        "runner homeboy_path updated from `/home/user/.cargo/bin/homeboy-0.229.1` to `homeboy`"
    ));
}

#[test]
fn realigns_stale_lab_workspace_homeboy_path_after_upgrade_failure() {
    let _local_version = pin_local_version_for_fixtures();
    let stale_path =
        "/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy";
    let runner = ssh_runner("lab", Some(stale_path));
    let mut commands = Vec::new();
    let mut path_updates = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => ("homeboy 0.229.11\n", "", 0),
                2 => (
                    "",
                    "source upgrade failed: fatal: not a git repository (or any of the parent directories): .git\n",
                    1,
                ),
                3 => ("homeboy 0.230.0\n", "", 0),
                4 => ("{\"success\":true}\n", "", 0),
                5 => ("homeboy 0.230.0\n", "", 0),
                _ => ("", "", 0),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
        |_runner, _path| unreachable!("source materialization not used"),
        |runner_id, homeboy_path| {
            path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
            Ok(())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].homeboy_path, "homeboy");
    assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.11"));
    assert_eq!(updated[0].new_version.as_deref(), Some("0.230.0"));
    assert_eq!(updated[0].bare_homeboy_version, None);
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(
        path_updates,
        vec![("lab".to_string(), "homeboy".to_string())]
    );
    assert_eq!(commands[1][0], stale_path);
    assert_eq!(commands[2], vec!["homeboy", "--version"]);
    assert_eq!(commands[3][0], "homeboy");
    assert!(updated[0]
        .detail
        .contains("after configured runner executable failed to upgrade"));
    assert!(updated[0].detail.contains(stale_path));
}

#[test]
fn source_runner_upgrade_realigns_to_materialized_source_build_when_path_shadows_old_homeboy() {
    let stale_path = "/home/user/Developer/_lab_workspaces/homeboy-old/target/debug/homeboy";
    let source_path = Path::new("/Users/user/Developer/homeboy@current-main");
    let remote_source = "/home/user/Developer/_lab_workspaces/homeboy-current-main";
    let source_binary = format!("{remote_source}/target/release/homeboy");
    let runner = ssh_runner("lab", Some(stale_path));
    let mut commands = Vec::new();
    let mut path_updates = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
        &[runner],
        true,
        Some(InstallMethod::Source),
        Some(source_path),
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => (
                    format!("homeboy {}+oldbuild\n", current_version()),
                    String::new(),
                    0,
                ),
                2 => ("prepared source checkout\n".to_string(), String::new(), 0),
                3 => ("{\"success\":true}\n".to_string(), String::new(), 0),
                4 => (
                    format!("homeboy {}+oldbuild\n", current_version()),
                    String::new(),
                    0,
                ),
                5 => (
                    format!("homeboy {}+oldbuild\n", current_version()),
                    String::new(),
                    0,
                ),
                6 if options.command[0] == source_binary => {
                    (format!("homeboy {}\n", current_version()), String::new(), 0)
                }
                7 if options.command[0] == source_binary => (
                    format!("{}\n", build_identity::current().display),
                    String::new(),
                    0,
                ),
                other => (
                    String::new(),
                    format!("unexpected command {other}: {:?}", options.command),
                    1,
                ),
            };
            Ok((
                exec_output(runner_id, options.command, &stdout, &stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
        |runner, path| {
            assert_eq!(runner.id, "lab");
            assert_eq!(path, source_path);
            Ok(remote_source.to_string())
        },
        |runner_id, homeboy_path| {
            path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
            Ok(())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].homeboy_path, source_binary);
    assert_eq!(
        updated[0].previous_version.as_deref(),
        Some(current_version())
    );
    assert_eq!(updated[0].new_version.as_deref(), Some(current_version()));
    assert_eq!(updated[0].bare_homeboy_version, None);
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(path_updates, vec![("lab".to_string(), source_binary)]);
    assert_eq!(&commands[1][..2], &["sh".to_string(), "-lc".to_string()]);
    assert_eq!(commands[2][0], stale_path);
    assert_eq!(commands[2][commands[2].len() - 2], "--source-path");
    assert_eq!(commands[2][commands[2].len() - 1], remote_source);
    assert!(updated[0].detail.contains("to source-built"));
}

#[test]
fn source_runner_upgrade_realigns_to_same_version_source_checkout_identity() {
    let source_dir = git_source_checkout();
    let expected_identity = source_checkout_build_identity(source_dir.path()).unwrap();
    assert_ne!(expected_identity, build_identity::current().display);

    let stale_path = "/home/user/Developer/_lab_workspaces/homeboy-old/target/debug/homeboy";
    let remote_source = "/home/user/Developer/_lab_workspaces/homeboy-current-main";
    let source_binary = format!("{remote_source}/target/release/homeboy");
    let runner = ssh_runner("lab", Some(stale_path));
    let mut commands = Vec::new();
    let mut path_updates = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
        &[runner],
        true,
        Some(InstallMethod::Source),
        Some(source_dir.path()),
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => (
                    format!("homeboy {}+oldbuild\n", current_version()),
                    String::new(),
                    0,
                ),
                2 => ("prepared source checkout\n".to_string(), String::new(), 0),
                3 => ("{\"success\":true}\n".to_string(), String::new(), 0),
                4 => (
                    format!("homeboy {}+oldbuild\n", current_version()),
                    String::new(),
                    0,
                ),
                5 => (
                    format!("homeboy {}+oldbuild\n", current_version()),
                    String::new(),
                    0,
                ),
                6 if options.command[0] == source_binary => {
                    (format!("homeboy {}\n", current_version()), String::new(), 0)
                }
                7 if options.command[0] == source_binary => {
                    (format!("{expected_identity}\n"), String::new(), 0)
                }
                other => (
                    String::new(),
                    format!("unexpected command {other}: {:?}", options.command),
                    1,
                ),
            };
            Ok((
                exec_output(runner_id, options.command, &stdout, &stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
        |runner, path| {
            assert_eq!(runner.id, "lab");
            assert_eq!(path, source_dir.path());
            Ok(remote_source.to_string())
        },
        |runner_id, homeboy_path| {
            path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
            Ok(())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].homeboy_path, source_binary);
    assert_eq!(updated[0].new_version.as_deref(), Some(current_version()));
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(path_updates, vec![("lab".to_string(), source_binary)]);
}

#[test]
fn runner_source_prepare_fast_forwards_attached_upstream_branch() {
    let fixture = remote_source_fixture();
    let checkout = fixture.root.path().join("attached");
    run_git(
        fixture.root.path(),
        &[
            "clone",
            fixture.origin.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    let expected_head = add_remote_commit(&fixture.seed, "remote update");

    run_source_prepare_script(&checkout);

    assert_eq!(git_stdout(&checkout, &["branch", "--show-current"]), "main");
    assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), expected_head);
    assert_eq!(
        git_stdout(&checkout, &["rev-parse", "origin/main"]),
        expected_head
    );
}

#[test]
fn runner_source_prepare_updates_detached_checkout_to_remote_default() {
    let fixture = remote_source_fixture();
    let checkout = fixture.root.path().join("detached");
    run_git(
        fixture.root.path(),
        &[
            "clone",
            fixture.origin.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    run_git(&checkout, &["checkout", "--detach", "HEAD"]);
    let expected_head = add_remote_commit(&fixture.seed, "remote update");

    run_source_prepare_script(&checkout);

    assert_eq!(git_stdout(&checkout, &["branch", "--show-current"]), "");
    assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), expected_head);
}

#[test]
fn runner_source_prepare_detached_checkout_ignores_default_branch_checked_out_elsewhere() {
    let fixture = remote_source_fixture();
    let primary = fixture.root.path().join("primary");
    let detached = fixture.root.path().join("detached-worktree");
    run_git(
        fixture.root.path(),
        &[
            "clone",
            fixture.origin.to_str().unwrap(),
            primary.to_str().unwrap(),
        ],
    );
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "--detach",
            detached.to_str().unwrap(),
            "HEAD",
        ],
    );
    let expected_head = add_remote_commit(&fixture.seed, "remote update");

    run_source_prepare_script(&detached);

    assert_eq!(git_stdout(&detached, &["branch", "--show-current"]), "");
    assert_eq!(git_stdout(&detached, &["rev-parse", "HEAD"]), expected_head);
    assert_eq!(git_stdout(&primary, &["branch", "--show-current"]), "main");
}

#[test]
fn realigns_versioned_runner_homeboy_path_using_final_bare_homeboy_state() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy-0.229.1"));
    let mut commands = Vec::new();
    let mut path_updates = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.229.1\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.229.1\n",
                4 => "homeboy 0.228.22\n",
                5 => "homeboy 0.229.6\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
        |_runner, _path| unreachable!("source materialization not used"),
        |runner_id, homeboy_path| {
            path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
            Ok(())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].homeboy_path, "homeboy");
    assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.1"));
    assert_eq!(updated[0].new_version.as_deref(), Some("0.229.6"));
    assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.229.6"));
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(
        path_updates,
        vec![("lab".to_string(), "homeboy".to_string())]
    );
    assert_eq!(
        commands[3],
        vec!["homeboy", "--version"],
        "the first bare probe reproduces the stale pre-upgrade state"
    );
    assert_eq!(
        commands[4],
        vec!["homeboy", "--version"],
        "final drift detection re-checks the remote bare binary"
    );
    assert!(updated[0].detail.contains("bare `homeboy` reports 0.229.6"));
    assert!(!updated[0].detail.contains("0.228.22"));
}

#[test]
fn realigns_versioned_runner_homeboy_path_when_only_final_bare_probe_succeeds() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.cargo/bin/homeboy-0.229.5"));
    let mut commands = Vec::new();
    let mut path_updates = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => ("homeboy 0.229.6\n", "", 0),
                2 => ("{\"success\":true}\n", "", 0),
                3 => ("homeboy 0.229.6\n", "", 0),
                4 => ("", "homeboy unavailable during remote upgrade\n", 1),
                5 => ("homeboy 0.229.7\n", "", 0),
                _ => ("", "", 0),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
        |_runner, _path| unreachable!("source materialization not used"),
        |runner_id, homeboy_path| {
            path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
            Ok(())
        },
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].success);
    assert!(updated[0].upgraded);
    assert_eq!(updated[0].homeboy_path, "homeboy");
    assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.6"));
    assert_eq!(updated[0].new_version.as_deref(), Some("0.229.7"));
    assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.229.7"));
    assert_eq!(updated[0].path_drift, None);
    assert_eq!(
        path_updates,
        vec![("lab".to_string(), "homeboy".to_string())]
    );
    assert_eq!(commands[3], vec!["homeboy", "--version"]);
    assert_eq!(commands[4], vec!["homeboy", "--version"]);
    assert!(updated[0].detail.contains(
        "runner homeboy_path updated from `/home/user/.cargo/bin/homeboy-0.229.5` to `homeboy`"
    ));
    assert!(!updated[0].detail.contains("runner PATH drift detected"));
}

#[test]
fn reports_exact_runner_set_remediation_when_path_update_is_unsafe() {
    let runner = ssh_runner("lab", Some("/opt/homeboy/custom-homeboy"));
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => "homeboy 0.229.1\n",
                2 => "{\"success\":true}\n",
                3 => "homeboy 0.229.1\n",
                4 => "homeboy 0.229.3\n",
                _ => "",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].homeboy_path, "/opt/homeboy/custom-homeboy");
    assert!(skipped[0]
        .path_drift
        .as_deref()
        .unwrap()
        .contains("automatic runner homeboy_path update is unsafe"));
    assert!(skipped[0].recovery_commands.contains(
        &"homeboy runner exec lab --ssh -- sh -lc 'type -a homeboy; command -v homeboy; homeboy --version'".to_string()
    ));
    assert!(skipped[0]
        .recovery_commands
        .contains(&"homeboy runner set lab --json '{\"homeboy_path\":\"homeboy\"}'".to_string()));
    assert!(skipped[0]
        .detail
        .contains("homeboy runner set lab --json '{\"homeboy_path\":\"homeboy\"}'"));
}

#[test]
fn restarts_stale_connected_daemon_after_runner_upgrade() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", None);
    let mut reconnects = Vec::new();

    let (updated, skipped) =
        upgrade_runners_with_executor_source_materializer_path_updater_and_reconnector(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                let stdout = match options.command.as_slice() {
                    [_, flag] if flag == "--version" => "homeboy 0.228.5\n",
                    _ => "{\"success\":true}\n",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            stale_runner_status,
            |runner_id| {
                reconnects.push(runner_id.to_string());
                Ok(
                    "connected runner daemon restarted after upgrade; session reports 0.228.5"
                        .to_string(),
                )
            },
            |_runner, _path| unreachable!("source materialization not used"),
            |_runner_id, _homeboy_path| unreachable!("homeboy_path update not used"),
        );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert_eq!(reconnects, vec!["lab".to_string()]);
    assert_eq!(updated[0].stale_daemon, None);
    assert!(updated[0]
        .detail
        .contains("connected runner daemon restarted after upgrade"));
}

fn extension_update(extension_id: &str, source_revision: &str) -> ExtensionUpgradeEntry {
    ExtensionUpgradeEntry {
        extension_id: extension_id.to_string(),
        old_version: "1.0.0".to_string(),
        new_version: "1.0.0".to_string(),
        linked: true,
        source_path: Some(format!(
            "/Users/user/Developer/homeboy-extensions/{extension_id}"
        )),
        git_root: Some("/Users/user/Developer/homeboy-extensions".to_string()),
        source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
        source_revision: Some(source_revision.to_string()),
        source_update: Default::default(),
    }
}

fn runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: false,
        state: RunnerSessionState::Disconnected,
        session: None,
        stale_daemon: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        session_path: "/tmp/homeboy-runner-session.json".to_string(),
    })
}

fn stale_runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: true,
        state: RunnerSessionState::Connected,
        session: None,
        stale_daemon: Some(RunnerStaleDaemonWarning::new(
            runner_id,
            "0.228.4".to_string(),
            "0.228.5".to_string(),
            None,
            None,
        )),
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        session_path: "/tmp/homeboy-runner-session.json".to_string(),
    })
}

fn ssh_runner(id: &str, homeboy_path: Option<&str>) -> Runner {
    Runner {
        id: id.to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some(format!("{id}-server")),
        workspace_root: Some("/home/user/workspace".to_string()),
        settings: RunnerSettings {
            homeboy_path: homeboy_path.map(str::to_string),
            ..Default::default()
        },
        env: HashMap::new(),
        secret_env: HashMap::new(),
        resources: HashMap::new(),
        policy: Default::default(),
    }
}

fn git_source_checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README.md"), "test\n").expect("write readme");
    run_git(dir.path(), &["init"]);
    run_git(
        dir.path(),
        &["config", "user.email", "homeboy@example.test"],
    );
    run_git(dir.path(), &["config", "user.name", "Homeboy Test"]);
    // Disable commit signing for the throwaway checkout. CI/dev environments
    // that set `commit.gpgsign = true` globally would otherwise fail the
    // commit (or leave the tree in a state where `rev-parse`/`status`
    // misbehave), causing `source_checkout_build_identity` to return `None`
    // and panic the caller's `.unwrap()`.
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "tag.gpgsign", "false"]);
    run_git(dir.path(), &["add", "README.md"]);
    run_git(
        dir.path(),
        &["commit", "--no-gpg-sign", "-m", "Initial commit"],
    );
    dir
}

struct RemoteSourceFixture {
    root: tempfile::TempDir,
    seed: PathBuf,
    origin: PathBuf,
}

fn remote_source_fixture() -> RemoteSourceFixture {
    let root = tempfile::tempdir().expect("tempdir");
    let seed = root.path().join("seed");
    let origin = root.path().join("origin.git");

    std::fs::create_dir_all(&seed).expect("mkdir seed");
    std::fs::write(seed.join("README.md"), "initial\n").expect("write readme");
    run_git(&seed, &["init", "-b", "main"]);
    configure_test_git_identity(&seed);
    run_git(&seed, &["add", "README.md"]);
    run_git(&seed, &["commit", "--no-gpg-sign", "-m", "Initial commit"]);
    run_git(
        root.path(),
        &[
            "clone",
            "--bare",
            seed.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    run_git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );

    RemoteSourceFixture { root, seed, origin }
}

fn configure_test_git_identity(path: &Path) {
    run_git(path, &["config", "user.email", "homeboy@example.test"]);
    run_git(path, &["config", "user.name", "Homeboy Test"]);
    run_git(path, &["config", "commit.gpgsign", "false"]);
    run_git(path, &["config", "tag.gpgsign", "false"]);
}

fn add_remote_commit(seed: &Path, message: &str) -> String {
    let file = seed.join("README.md");
    let mut content = std::fs::read_to_string(&file).expect("read readme");
    content.push_str(message);
    content.push('\n');
    std::fs::write(&file, content).expect("write readme");
    run_git(seed, &["add", "README.md"]);
    run_git(seed, &["commit", "--no-gpg-sign", "-m", message]);
    run_git(seed, &["push", "origin", "main"]);
    git_stdout(seed, &["rev-parse", "HEAD"])
}

fn run_source_prepare_script(path: &Path) {
    let output = Command::new("sh")
        .arg("-lc")
        .arg(runner_source_checkout_prepare_script())
        .current_dir(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run source prepare script");
    assert!(
        output.status.success(),
        "source prepare script failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        // Isolate from ambient global/system git config so the throwaway
        // checkout behaves deterministically regardless of the host
        // environment (e.g. dubious-ownership safe.directory checks or
        // global signing settings).
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn exec_output(
    runner_id: &str,
    argv: Vec<String>,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: runner_id.to_string(),
        dry_run: false,
        mode: RunnerExecMode::DiagnosticSsh,
        argv,
        remote_cwd: "/home/user/workspace".to_string(),
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: None,
        job_events: None,
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}
