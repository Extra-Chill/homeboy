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
        vec![RunnerRequiredTool::homeboy()]
    );
    assert_eq!(capability_plan.command, "homeboy upgrade");
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
fn runner_source_prepare_preserves_detached_source_identity() {
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
    let expected_head = git_stdout(&checkout, &["rev-parse", "HEAD"]);
    add_remote_commit(&fixture.seed, "remote update");

    run_source_prepare_script(&checkout);

    assert_eq!(git_stdout(&checkout, &["branch", "--show-current"]), "");
    assert_eq!(git_stdout(&checkout, &["rev-parse", "HEAD"]), expected_head);
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
fn runner_upgrade_detail_reports_selected_binary_and_reconnect_requirement() {
    let _local_version = pin_local_version_for_fixtures();
    let runner = ssh_runner("lab", Some("/home/user/.local/bin/homeboy"));

    let (updated, skipped) = upgrade_runners_with_executor(
        &[runner],
        false,
        None,
        None,
        &[],
        |runner_id, options| {
            let stdout = match options.command.as_slice() {
                [_, flag] if flag == "--version" => "homeboy 0.199.2\n",
                _ => "{\"success\":true}\n",
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
        runner_status,
    );

    assert!(skipped.is_empty());
    assert_eq!(updated.len(), 1);
    assert!(updated[0].detail.contains(
        "selected runner binary path: /home/user/.local/bin/homeboy; reconnect required: no"
    ));
}
