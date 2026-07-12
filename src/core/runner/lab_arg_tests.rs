use super::super::lab_args::{
    lab_offload_source_path, rewrite_lab_offload_args, rewrite_runner_resident_lab_offload_args,
    LabPathRemap, EXPLICIT_PASSTHROUGH_SENTINEL,
};

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn rewrites_lab_offload_path_and_strips_runner_and_output_flags() {
    let input = args(&[
        "homeboy",
        "audit",
        "--path",
        "/Users/user/Developer/project",
        "--runner",
        "lab",
        "--json-summary",
        "--output",
        "/tmp/local.json",
        "--runner=other",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "audit",
            "--path",
            "/home/user/Developer/project",
            "--json-summary",
        ])
    );
}

#[test]
fn maps_command_output_path_to_runner_output_path() {
    let input = args(&[
        "homeboy",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "controller",
        "run-from-spec",
        "loop.json",
        "--max-actions",
        "1",
        "--output",
        "/tmp/local-result.json",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(
            &input,
            "/home/user/Developer/project",
            &[],
            Some("/home/user/Developer/project/homeboy-lab-structured-output.json"),
        ),
        args(&[
            "homeboy",
            "--force-hot",
            "agent-task",
            "controller",
            "run-from-spec",
            "loop.json",
            "--max-actions",
            "1",
            "--output",
            "/home/user/Developer/project/homeboy-lab-structured-output.json",
        ])
    );
}

#[test]
fn strips_controller_artifact_root_from_lab_offload_command() {
    let input = args(&[
        "homeboy",
        "fuzz",
        "run",
        "--path",
        "/Users/user/Developer/project",
        "--artifact-root",
        "/var/folders/local-homeboy-artifacts",
        "--workload",
        "smoke",
        "--artifact-root=/tmp/also-local",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "fuzz",
            "run",
            "--path",
            "/home/user/Developer/project",
            "--workload",
            "smoke",
        ])
    );
}

#[test]
fn strips_lab_only_flags_from_lab_offload_command() {
    let input = args(&[
        "homeboy",
        "fuzz",
        "run",
        "jetpack",
        "--rig",
        "jetpack-api-route-inventory",
        "--lab-only",
        "--no-local-execution",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/jetpack", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "fuzz",
            "run",
            "jetpack",
            "--rig",
            "jetpack-api-route-inventory",
        ])
    );
}

#[test]
fn strips_controller_artifact_root_from_runner_resident_command() {
    let input = args(&[
        "homeboy",
        "agent-task",
        "status",
        "agent-task-123",
        "--artifact-root",
        "/var/folders/local-homeboy-artifacts",
        "--artifact-root=/tmp/also-local",
    ]);

    assert_eq!(
        rewrite_runner_resident_lab_offload_args(&input, None),
        args(&[
            "homeboy",
            "--force-hot",
            "agent-task",
            "status",
            "agent-task-123",
        ])
    );
}

#[test]
fn leaves_relative_passthrough_path_args_untouched() {
    let input = args(&[
        "homeboy",
        "test",
        "--path=/Users/user/Developer/project",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        "--path",
        "test-fixture",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "test",
            "--path=/home/user/Developer/project",
            "--",
            "--path",
            "test-fixture",
        ])
    );
}

#[test]
fn remaps_synced_source_paths_in_passthrough_args() {
    let input = args(&[
        "homeboy",
        "bench",
        "static-site-importer",
        "--path",
        "/Users/user/Developer/static-site-importer@fix",
        "--runner",
        "homeboy-lab",
        "--lab-only",
        "--rig",
        "static-site-importer-fixture-matrix",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        "--static-site-importer-path",
        "/Users/user/Developer/static-site-importer@fix",
        "--batch-size",
        "1",
    ]);
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/static-site-importer@fix".to_string(),
        remote: "/home/user/_lab_workspaces/static-site-importer@fix".to_string(),
    }];

    assert_eq!(
        rewrite_lab_offload_args(
            &input,
            "/home/user/_lab_workspaces/static-site-importer@fix",
            &mappings,
            None,
        ),
        args(&[
            "homeboy",
            "--force-hot",
            "bench",
            "static-site-importer",
            "--path",
            "/home/user/_lab_workspaces/static-site-importer@fix",
            "--rig",
            "static-site-importer-fixture-matrix",
            "--",
            "--static-site-importer-path",
            "/home/user/_lab_workspaces/static-site-importer@fix",
            "--batch-size",
            "1",
        ])
    );
}

#[test]
fn preserves_repo_relative_changed_files_through_lab_dispatch_rewrite() {
    let input = args(&[
        "homeboy",
        "review",
        "lint",
        "--path",
        "/Users/user/Developer/project",
        "--changed-only",
        "--lab-changed-files-json",
        "[\"inc/Workspace/WorkspaceWorktreeCleanupEngine.php\",\"tests/worktree-retention-apply-protections.php\"]",
    ]);
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/project".to_string(),
        remote: "/runner/workspaces/project".to_string(),
    }];

    let rewritten = rewrite_lab_offload_args(&input, "/runner/workspaces/project", &mappings, None);

    assert_eq!(
        rewritten,
        args(&[
            "homeboy",
            "--force-hot",
            "review",
            "lint",
            "--path",
            "/runner/workspaces/project",
            "--changed-only",
            "--lab-changed-files-json",
            "[\"inc/Workspace/WorkspaceWorktreeCleanupEngine.php\",\"tests/worktree-retention-apply-protections.php\"]",
        ])
    );
}

#[test]
fn normalizes_changed_file_paths_to_the_remote_workspace_coordinate_system() {
    let input = args(&[
        "homeboy",
        "review",
        "lint",
        "--lab-changed-files-json",
        "[\"/Users/user/Developer/project/src/lib.rs\"]",
    ]);
    let mappings = vec![LabPathRemap {
        local: "/Users/user/Developer/project".to_string(),
        remote: "/runner/workspaces/project".to_string(),
    }];

    let rewritten = rewrite_lab_offload_args(&input, "/runner/workspaces/project", &mappings, None);

    assert_eq!(
        rewritten.last().map(String::as_str),
        Some("[\"src/lib.rs\"]")
    );
}

#[test]
fn leaves_unmapped_passthrough_source_paths_for_safety_guard() {
    let input = args(&[
        "homeboy",
        "bench",
        "static-site-importer",
        "--path",
        "/Users/user/Developer/static-site-importer@fix",
        "--",
        "--static-site-importer-path",
        "/Users/user/Developer/static-site-importer@fix",
    ]);

    let rewritten = rewrite_lab_offload_args(
        &input,
        "/home/user/_lab_workspaces/static-site-importer@fix",
        &[],
        None,
    );

    assert!(rewritten
        .iter()
        .any(|arg| arg == "/Users/user/Developer/static-site-importer@fix"));
}

#[test]
fn strips_internal_passthrough_sentinel_from_lab_offload_command() {
    let filter = "--filter=ConversationStoreFactoryTest::test_canonical_conversation_session_abilities_route_through_swapped_store";
    let input = args(&[
        "homeboy",
        "test",
        "sample-plugin",
        "--path",
        "/Users/user/Developer/sample-plugin@fix",
        "--",
        EXPLICIT_PASSTHROUGH_SENTINEL,
        filter,
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/sample-plugin@fix", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "test",
            "sample-plugin",
            "--path",
            "/home/user/Developer/sample-plugin@fix",
            "--",
            filter,
        ])
    );
}

#[test]
fn rewrite_lab_offload_args_does_not_duplicate_force_hot() {
    let input = args(&[
        "homeboy",
        "--force-hot",
        "refactor",
        "--from",
        "audit",
        "--path",
        "/Users/user/Developer/project",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/project", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "refactor",
            "--from",
            "audit",
            "--path",
            "/home/user/Developer/project",
        ])
    );
}

#[test]
fn rewrite_lab_offload_args_preserves_extension_dev_run_runner() {
    let input = args(&[
        "homeboy",
        "extension",
        "dev-run",
        "--source",
        "/Users/user/Developer/homeboy-extensions/wordpress",
        "--runner",
        "homeboy-lab",
        "wordpress",
        "homeboy",
        "extension",
        "show",
        "wordpress",
    ]);

    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/homeboy", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "extension",
            "dev-run",
            "--source",
            "/Users/user/Developer/homeboy-extensions/wordpress",
            "--runner",
            "homeboy-lab",
            "wordpress",
            "homeboy",
            "extension",
            "show",
            "wordpress",
        ])
    );
}

#[test]
fn lab_offload_source_path_uses_extension_refresh_local_source() {
    let dir = tempfile::tempdir().expect("extension source");
    let args = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "refresh".to_string(),
        dir.path().display().to_string(),
        "--id".to_string(),
        "nodejs".to_string(),
    ];

    let source = lab_offload_source_path(&args).expect("source path");

    assert_eq!(source, dir.path());
}

#[test]
fn lab_offload_source_path_ignores_extension_refresh_git_source() {
    let cwd = std::env::current_dir().expect("cwd");
    let args = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "refresh".to_string(),
        "https://example.test/extensions.git".to_string(),
        "--id".to_string(),
        "nodejs".to_string(),
    ];

    let source = lab_offload_source_path(&args).expect("source path");

    assert_eq!(source, cwd);
}

#[test]
fn detects_lab_offload_source_path_from_path_flag() {
    let input = args(&["homeboy", "test", "--path", "/Users/user/Developer/project"]);

    assert_eq!(
        lab_offload_source_path(&input).expect("path"),
        std::path::PathBuf::from("/Users/user/Developer/project")
    );
}

#[test]
fn rig_check_lab_offload_uses_explicit_component_path_as_source() {
    let input = args(&[
        "homeboy",
        "rig",
        "check",
        "woocommerce-performance",
        "--path",
        "/Users/user/Developer/woocommerce",
        "--runner",
        "homeboy-lab",
        "--lab-only",
    ]);

    assert_eq!(
        lab_offload_source_path(&input).expect("rig check source path"),
        std::path::PathBuf::from("/Users/user/Developer/woocommerce")
    );
    assert_eq!(
        rewrite_lab_offload_args(&input, "/home/user/Developer/woocommerce", &[], None),
        args(&[
            "homeboy",
            "--force-hot",
            "rig",
            "check",
            "woocommerce-performance",
            "--path",
            "/home/user/Developer/woocommerce",
        ])
    );
}
