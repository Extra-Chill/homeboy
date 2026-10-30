use super::*;

#[test]
fn matching_runner_extension_revisions_need_only_one_exec() {
    let runner = ssh_runner("lab", Some("homeboy"));
    let updates = vec![
        extension_update("swift", "98a61eda12345678"),
        extension_update("wordpress", "48517ac3"),
    ];
    let mut commands = Vec::new();
    let (synced, skipped, failed) = sync_runner_extensions(
        &runner,
        "homeboy",
        &updates,
        &mut |runner_id, options| {
            commands.push(options.command.clone());
            Ok((
                exec_output(
                    runner_id,
                    options.command,
                    r#"{"success":true,"data":{"entities":[{"id":"swift","source_revision":"98a61eda"},{"id":"wordpress","source_revision":"48517ac3"}]}}"#,
                    "",
                    0,
                ),
                0,
            ))
        },
    );
    assert!(failed.is_empty());
    assert!(skipped.is_empty());
    assert_eq!(synced.len(), 2);
    assert!(synced.iter().all(|entry| entry.synced
        && entry
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("already at revision"))));
    assert_eq!(commands, vec![vec!["homeboy", "extension", "list"]]);
}

#[test]
fn runner_extension_sync_installs_only_changed_or_unknown_revisions() {
    let runner = ssh_runner("lab", Some("homeboy"));
    let updates = vec![
        extension_update("current", "abcdef123456"),
        extension_update("changed", "123456abcdef"),
        extension_update("unknown", "123456abcdef"),
        extension_update("missing", "123456abcdef"),
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
    assert_eq!(commands.len(), 4);
    for (command, id) in commands[1..].iter().zip(["changed", "unknown", "missing"]) {
        assert_eq!(command[2], "install");
        assert_eq!(command[5], id);
        assert_eq!(command.last().unwrap(), "--replace");
    }
}
