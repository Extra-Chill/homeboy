use super::*;

#[test]
fn runner_extension_sync_installs_only_changed_or_unknown_revisions() {
    let runner = ssh_runner("lab", Some("homeboy"));
    let full_sha = "123456abcdef123456abcdef123456abcdef1234";
    let updates = vec![
        extension_update("current", "abcdef123456"),
        extension_update("changed", full_sha),
        extension_update("unknown", full_sha),
        extension_update("missing", full_sha),
    ];
    let mut commands = Vec::new();
    let (synced, skipped, failed) = sync_runner_extensions(
        &runner,
        "homeboy",
        &updates,
        &mut |runner_id, options| {
            commands.push(options.command.clone());
            let stdout = if commands.len() == 1 {
                r#"{"success":true,"data":{"entities":[{"id":"current","source_revision":"abcdef1"},{"id":"changed","source_revision":"abcdef1"},{"id":"unknown","source_revision":""}]}}"#
            } else {
                ""
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        },
    );
    assert_eq!(synced.len(), 4);
    assert!(skipped.is_empty());
    assert!(failed.is_empty());
    // A revision-prefix match is reported synced without an install.
    assert!(synced[0]
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("already at revision")));
    assert!(synced[1..]
        .iter()
        .all(|entry| entry.source_revision == full_sha && entry.detail.is_none()));
    // The unchanged extension costs no job; the three changed ones share one
    // source and revision, so one runner job installs them from one checkout.
    assert_eq!(
        commands[1..],
        [vec![
            "homeboy",
            "extension",
            "install",
            "https://github.com/Extra-Chill/homeboy-extensions.git",
            "--id",
            "changed",
            "--id",
            "unknown",
            "--id",
            "missing",
            "--ref",
            full_sha,
            "--replace",
        ]]
    );
}

#[test]
fn failed_batch_install_falls_back_to_exact_per_extension_installs() {
    let runner = ssh_runner("lab", Some("homeboy"));
    let updates = vec![
        extension_update("a", "1111111"),
        extension_update("b", "1111111"),
    ];
    let mut commands = Vec::new();
    let (synced, _, failed) =
        sync_runner_extensions(&runner, "homeboy", &updates, &mut |runner_id, options| {
            commands.push(options.command.clone());
            let ids = options.command.iter().filter(|arg| *arg == "--id").count();
            let (stdout, code) = match (commands.len(), ids) {
                (1, _) => (r#"{"success":true,"data":{"entities":[]}}"#, 0),
                // An older runner rejects the batch; then `b` alone fails.
                (_, 2) => ("", 2),
                _ if options.command.contains(&"b".to_string()) => ("", 1),
                _ => ("", 0),
            };
            Ok((
                exec_output(runner_id, options.command, stdout, "", code),
                code,
            ))
        });
    assert_eq!(commands.len(), 4);
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].extension_id, "a");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].extension_id, "b");
}

#[test]
fn runner_extension_sync_is_admitted_as_convergence_not_bulk_workload() {
    let runner = ssh_runner("lab", Some("homeboy"));
    let updates = vec![extension_update("wordpress", "123456abcdef")];
    let mut admission_classes = Vec::new();
    let (synced, _, failed) =
        sync_runner_extensions(&runner, "homeboy", &updates, &mut |runner_id, options| {
            admission_classes.push(
                options
                    .env
                    .get(homeboy_core::runner_pressure::ADMISSION_CLASS_ENV)
                    .cloned(),
            );
            let stdout = if admission_classes.len() == 1 {
                r#"{"success":true,"data":{"entities":[]}}"#
            } else {
                ""
            };
            Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
        });
    assert!(failed.is_empty());
    assert_eq!(synced.len(), 1);
    // Inventory and install both bypass the saturated Lab's pressure queue.
    assert_eq!(
        admission_classes,
        vec![Some(homeboy_core::runner_pressure::CONVERGENCE_ADMISSION_CLASS.to_string()); 2]
    );
}

#[test]
fn failed_runner_inventory_fails_every_sync_target_without_installing() {
    let runner = ssh_runner("lab", Some("homeboy"));
    let updates = vec![
        extension_update("a", "1111111"),
        extension_update("b", "2222222"),
    ];
    let mut commands = Vec::new();
    let (synced, skipped, failed) =
        sync_runner_extensions(&runner, "homeboy", &updates, &mut |runner_id, options| {
            commands.push(options.command.clone());
            Ok((exec_output(runner_id, options.command, "", "boom", 3), 3))
        });
    assert!(synced.is_empty());
    assert!(skipped.is_empty());
    assert_eq!(commands, vec![vec!["homeboy", "extension", "list"]]);
    assert_eq!(failed.len(), 2);
    assert!(failed.iter().all(|entry| entry
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("runner extension inventory failed (3)"))
        && entry.recovery_commands.len() == 1));
}
