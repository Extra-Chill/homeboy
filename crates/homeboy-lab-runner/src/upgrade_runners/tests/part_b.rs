//! Split partition of tests (see mod.rs for shared setup).
#![cfg(test)]

use super::*;
use crate::{
    Runner, RunnerActiveJobState, RunnerExecMode, RunnerExecOutput, RunnerKind, RunnerRequiredTool,
    RunnerSessionState, RunnerStaleDaemonWarning, RunnerStatusReport,
};
use homeboy_core::build_identity;
use homeboy_core::server::RunnerSettings;
use homeboy_core::Result;
use homeboy_upgrade::upgrade::current_version;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use homeboy_upgrade::upgrade::InstallMethod;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
thread_local! {
    static LOCAL_VERSION_OVERRIDE: RefCell<Option<String>> = const { RefCell::new(None) };
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
    assert!(calls[1].0[2].contains("git rev-parse --verify HEAD"));
    assert!(!calls[1].0[2].contains("git fetch"));
    assert!(!calls[1].0[2].contains("git pull"));
    assert!(!calls[1].0[2].contains("git reset"));
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
                8 if options.command[0] == source_binary => {
                    (format!("{expected_identity}\n"), String::new(), 0)
                }
                9 if options.command[0] == source_binary => {
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
fn rejects_stale_source_runner_identity_before_extension_sync() {
    let source_dir = git_source_checkout();
    let expected_identity = source_checkout_build_identity(source_dir.path()).unwrap();
    let remote_source = "/home/user/Developer/_lab_workspaces/homeboy-current-main";
    let source_binary = format!("{remote_source}/target/release/homeboy");
    let runner = ssh_runner(
        "lab",
        Some("/home/user/Developer/homeboy@stale/target/release/homeboy"),
    );
    let extension_updates = vec![extension_update("required-extension", "48517ac3")];
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_and_source_materializer(
        &[runner],
        true,
        Some(InstallMethod::Source),
        Some(source_dir.path()),
        &extension_updates,
        |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = match commands.len() {
                1 => format!("homeboy {}+oldbuild\n", current_version()),
                2 => "prepared source checkout\n".to_string(),
                3 => "{\"success\":true}\n".to_string(),
                4 => format!("homeboy {}\n", current_version()),
                5 => format!("homeboy {}+stale\n", current_version()),
                6 if options.command[0] == source_binary => {
                    format!("homeboy {}\n", current_version())
                }
                7 if options.command[0] == source_binary => {
                    format!("homeboy {}+stale\n", current_version())
                }
                8 => format!("homeboy {}\n", current_version()),
                9 => format!("homeboy {}+stale\n", current_version()),
                10 => format!("homeboy {}+stale\n", current_version()),
                11 => format!("homeboy {}\n", current_version()),
                12 => format!("homeboy {}+stale\n", current_version()),
                13 => format!("homeboy {}+stale\n", current_version()),
                other => panic!("unexpected runner command {other}: {:?}", options.command),
            };
            Ok((exec_output(runner_id, options.command, &stdout, "", 0), 0))
        },
        runner_status,
        |_runner, _path| Ok(remote_source.to_string()),
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(!skipped[0].success);
    assert!(skipped[0]
        .path_drift
        .as_deref()
        .unwrap()
        .contains(&expected_identity));
    assert_eq!(skipped[0].extensions_synced.len(), 0);
    assert_eq!(skipped[0].extensions_skipped.len(), 1);
    assert!(skipped[0].extensions_skipped[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("did not converge"));
    assert!(!commands
        .iter()
        .any(|command| command.contains(&"extension".to_string())));
}

#[test]
fn reports_unrefreshable_extensions_when_runner_binary_drift_defers_sync() {
    let extensions = vec![ExtensionUpgradeEntry {
        extension_id: "local-extension".to_string(),
        old_version: "1.0.0".to_string(),
        new_version: "1.0.0".to_string(),
        linked: false,
        source_path: None,
        git_root: None,
        source_url: None,
        source_revision: None,
        source_update: Default::default(),
    }];

    let skipped = defer_runner_extensions_for_binary_drift(&extensions, "identity mismatch");

    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].extension_id, "local-extension");
    assert!(skipped[0]
        .detail
        .as_deref()
        .unwrap()
        .contains("unrefreshable"));
}

#[test]
fn rejects_packaged_runner_with_same_version_but_different_controller_identity() {
    let runner = ssh_runner("lab", Some("/opt/homeboy/homeboy"));
    let extensions = vec![extension_update("required-extension", "48517ac3")];
    let mut calls = 0;

    let entry = upgrade_runner_with_executor(
        &runner,
        true,
        Some(InstallMethod::Binary),
        None,
        &extensions,
        &mut |runner_id, options| {
            calls += 1;
            let stdout = match calls {
                1 | 3 => format!("homeboy {}\n", current_version()),
                2 => "{\"success\":true}\n".to_string(),
                _ => format!("homeboy {}+other-build\n", current_version()),
            };
            Ok((exec_output(runner_id, options.command, &stdout, "", 0), 0))
        },
        &runner_status,
        &mut |_| Ok("reconnected".to_string()),
        &mut |_, _| Ok("unused".to_string()),
        &mut |_, _| Ok(()),
        Some("controller-build"),
    );

    assert!(!entry.success);
    assert!(entry
        .path_drift
        .as_deref()
        .unwrap()
        .contains("controller-build"));
    assert_eq!(entry.extensions_synced.len(), 0);
    assert_eq!(entry.extensions_skipped.len(), 1);
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
    let expected_head = git_stdout(&detached, &["rev-parse", "HEAD"]);
    add_remote_commit(&fixture.seed, "remote update");

    run_source_prepare_script(&detached);

    assert_eq!(git_stdout(&detached, &["branch", "--show-current"]), "");
    assert_eq!(git_stdout(&detached, &["rev-parse", "HEAD"]), expected_head);
    assert_eq!(git_stdout(&primary, &["branch", "--show-current"]), "main");
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
