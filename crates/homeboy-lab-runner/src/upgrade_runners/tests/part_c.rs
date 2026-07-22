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
        vec!["homeboy runner exec --ssh lab -- /home/user/.cargo/bin/homeboy extension install https://github.com/Extra-Chill/homeboy-extensions.git --id swift --ref 98a61eda --replace"]
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
fn realigns_stale_source_checkout_homeboy_path_after_upgrade_failure() {
    let _local_version = pin_local_version_for_fixtures();
    let stale_path = "/home/user/Developer/homeboy@upgrade-bootstrap/target/release/homeboy";
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
                    "source upgrade command exited successfully but the active binary was not replaced\n",
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
fn runner_source_prepare_preserves_local_only_branch_identity() {
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
    run_git(&checkout, &["switch", "-c", "ops/install-7851"]);
    let expected_head = git_stdout(&checkout, &["rev-parse", "HEAD"]);
    add_remote_commit(&fixture.seed, "remote update");

    run_source_prepare_script(&checkout);

    assert_eq!(
        git_stdout(&checkout, &["branch", "--show-current"]),
        "ops/install-7851"
    );
    assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), expected_head);
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
fn source_prepare_failure_reports_clean_refresh_homeboy_target() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("homeboy lab", Some("/home/user/.cargo/bin/homeboy"));
    let source_path = Path::new("/Users/user/Developer/homeboy@detached-source");
    let mut commands = Vec::new();

    let (updated, skipped) = upgrade_runners_with_executor_and_source_materializer(
        &[runner],
        true,
        Some(InstallMethod::Source),
        Some(source_path),
        &[],
        |runner_id, options| {
            commands.push(options.command.clone());
            let (stdout, stderr, exit_code) = match commands.len() {
                1 => ("homeboy 0.228.13\n", "", 0),
                2 => (
                    "",
                    "source upgrade failed: You are not currently on a branch\n",
                    1,
                ),
                _ => ("", "unexpected command", 1),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, stderr, exit_code),
                exit_code,
            ))
        },
        runner_status,
        |_runner, _path| Ok("/home/user/workspace/homeboy-detached-source".to_string()),
    );

    assert!(updated.is_empty());
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0]
        .detail
        .contains("You are not currently on a branch"));
    assert!(skipped[0].detail.contains(
        "homeboy runner refresh-homeboy 'homeboy lab' --ref main --target-dir /home/user/workspace/_homeboy_binaries/homeboy-main --reconnect"
    ));
}
