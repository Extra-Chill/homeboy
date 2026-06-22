use super::args::{EditArgs, FileModifications, LineOperations, PatternOperations};
use super::{run, FileArgs, FileCommand, FileCommandOutput};
use crate::commands::GlobalArgs;
use crate::test_support::with_isolated_home;
use std::path::Path;

fn write_project_config(home: &Path, project_id: &str, project_root: &Path) {
    let project_dir = home
        .join(".config")
        .join("homeboy")
        .join("projects")
        .join(project_id);
    std::fs::create_dir_all(&project_dir).expect("create project config dir");
    let config = serde_json::json!({
        "base_path": project_root.to_string_lossy(),
    });
    std::fs::write(
        project_dir.join(format!("{project_id}.json")),
        serde_json::to_vec(&config).expect("serialize project config"),
    )
    .expect("write project config");
}

#[test]
fn file_read_json_includes_size_metadata() {
    let project_root = tempfile::tempdir().expect("project tempdir");
    let project_id = "local-file-read";
    let content = "hello\nworld";

    std::fs::write(project_root.path().join("sample.txt"), content).expect("write sample file");

    let result = with_isolated_home(|home| {
        write_project_config(home.path(), project_id, project_root.path());

        run(
            FileArgs {
                command: FileCommand::Read {
                    project_id: project_id.to_string(),
                    path: "sample.txt".to_string(),
                    raw: false,
                },
            },
            &GlobalArgs {},
        )
    });

    let (output, code) = result.expect("run homeboy file read");
    let FileCommandOutput::Standard(payload) = output else {
        panic!("expected standard file output");
    };
    let expected_path = project_root
        .path()
        .join("sample.txt")
        .to_string_lossy()
        .to_string();

    assert_eq!(code, 0);
    assert_eq!(payload.command, "file.read");
    assert_eq!(payload.path.as_deref(), Some(expected_path.as_str()));
    assert_eq!(payload.content.as_deref(), Some(content));
    assert_eq!(payload.size, Some(content.len() as i64));
}

#[test]
fn file_delete_without_apply_returns_plan_and_preserves_file() {
    let project_root = tempfile::tempdir().expect("project tempdir");
    let project_id = "local-file-delete-plan";
    let file_path = project_root.path().join("sample.txt");
    std::fs::write(&file_path, "keep me").expect("write sample file");

    let result = with_isolated_home(|home| {
        write_project_config(home.path(), project_id, project_root.path());

        run(
            FileArgs {
                command: FileCommand::Delete {
                    project_id: project_id.to_string(),
                    path: "sample.txt".to_string(),
                    recursive: false,
                    apply: false,
                },
            },
            &GlobalArgs {},
        )
    });

    let (output, code) = result.expect("run homeboy file delete");
    let FileCommandOutput::Standard(payload) = output else {
        panic!("expected standard file output");
    };

    assert_eq!(code, 0);
    assert_eq!(payload.command, "file.delete");
    assert!(payload.dry_run);
    assert_eq!(
        payload.action_required.as_deref(),
        Some("Re-run with --apply to delete the remote path.")
    );
    assert!(file_path.exists());
}

#[test]
fn file_edit_dry_run_returns_preview_and_preserves_file() {
    let project_root = tempfile::tempdir().expect("project tempdir");
    let project_id = "local-file-edit-dry-run";
    let file_path = project_root.path().join("sample.txt");
    std::fs::write(&file_path, "one\ntwo\nthree").expect("write sample file");

    let result = with_isolated_home(|home| {
        write_project_config(home.path(), project_id, project_root.path());

        run(
            FileArgs {
                command: FileCommand::Edit(EditArgs {
                    project_id: project_id.to_string(),
                    file_path: "sample.txt".to_string(),
                    dry_run: true,
                    force: false,
                    line_ops: LineOperations {
                        replace_line: Some(2),
                        replace_line_content: Some("TWO".to_string()),
                        ..Default::default()
                    },
                    pattern_ops: PatternOperations::default(),
                    file_mods: FileModifications::default(),
                }),
            },
            &GlobalArgs {},
        )
    });

    let (output, code) = result.expect("run homeboy file edit dry-run");
    let FileCommandOutput::Edit(payload) = output else {
        panic!("expected edit file output");
    };

    assert_eq!(code, 0);
    assert_eq!(payload.command, "file.edit");
    assert!(payload.dry_run);
    assert_eq!(payload.change_count, 1);
    assert_eq!(payload.changes_made[0].line_number, 2);
    assert_eq!(payload.changes_made[0].original, "two");
    assert_eq!(payload.changes_made[0].modified, "TWO");
    assert_eq!(
        std::fs::read_to_string(&file_path).expect("read sample file"),
        "one\ntwo\nthree"
    );
}

#[test]
fn file_edit_without_dry_run_writes_file() {
    let project_root = tempfile::tempdir().expect("project tempdir");
    let project_id = "local-file-edit-write";
    let file_path = project_root.path().join("sample.txt");
    std::fs::write(&file_path, "one\ntwo\nthree").expect("write sample file");

    let result = with_isolated_home(|home| {
        write_project_config(home.path(), project_id, project_root.path());

        run(
            FileArgs {
                command: FileCommand::Edit(EditArgs {
                    project_id: project_id.to_string(),
                    file_path: "sample.txt".to_string(),
                    dry_run: false,
                    force: false,
                    line_ops: LineOperations {
                        replace_line: Some(2),
                        replace_line_content: Some("TWO".to_string()),
                        ..Default::default()
                    },
                    pattern_ops: PatternOperations::default(),
                    file_mods: FileModifications::default(),
                }),
            },
            &GlobalArgs {},
        )
    });

    let (output, code) = result.expect("run homeboy file edit");
    let FileCommandOutput::Edit(payload) = output else {
        panic!("expected edit file output");
    };

    assert_eq!(code, 0);
    assert!(!payload.dry_run);
    assert_eq!(payload.change_count, 1);
    assert_eq!(
        std::fs::read_to_string(&file_path).expect("read sample file"),
        "one\nTWO\nthree"
    );
}

#[test]
fn file_edit_force_allows_first_replacement_when_pattern_has_multiple_matches() {
    let project_root = tempfile::tempdir().expect("project tempdir");
    let project_id = "local-file-edit-force";
    let file_path = project_root.path().join("sample.txt");
    std::fs::write(&file_path, "needle\nneedle").expect("write sample file");

    let result = with_isolated_home(|home| {
        write_project_config(home.path(), project_id, project_root.path());

        run(
            FileArgs {
                command: FileCommand::Edit(EditArgs {
                    project_id: project_id.to_string(),
                    file_path: "sample.txt".to_string(),
                    dry_run: false,
                    force: true,
                    line_ops: LineOperations::default(),
                    pattern_ops: PatternOperations {
                        replace_pattern: Some("needle".to_string()),
                        replace_pattern_content: Some("thread".to_string()),
                        ..Default::default()
                    },
                    file_mods: FileModifications::default(),
                }),
            },
            &GlobalArgs {},
        )
    });

    let (_output, code) = result.expect("run forced homeboy file edit");

    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&file_path).expect("read sample file"),
        "thread\nneedle"
    );
}
