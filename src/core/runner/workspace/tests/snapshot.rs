use std::fs;
use std::path::Path;

use crate::core::runner::workspace::snapshot::{snapshot_archive_command, snapshot_install_command};
use crate::core::runner::workspace::sync::sync_workspace;
use crate::core::runner::workspace::types::{
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

#[test]
fn runner_snapshot_includes_override_generated_output_excludes() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("packages/cli/dist")).expect("dist dir");
        fs::write(
            source.path().join("packages/cli/dist/homeboy.js"),
            "built\n",
        )
        .expect("built output");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-includes","kind":"local","workspace_root":"{}","policy":{{"snapshot_includes":["packages/cli/dist","packages/cli/dist/**"]}}}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local-includes",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");

        assert_eq!(exit_code, 0);
        assert!(output
            .includes
            .contains(&"packages/cli/dist/**".to_string()));
        assert!(!output.excludes.contains(&"dist".to_string()));
        assert!(Path::new(&output.remote_path)
            .join("packages/cli/dist/homeboy.js")
            .exists());
    });
}

#[test]
fn runner_snapshot_excludes_extend_default_snapshot_policy() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("src")).expect("src dir");
        fs::create_dir_all(source.path().join("generated-state")).expect("state dir");
        fs::write(source.path().join("src/source.txt"), "source\n").expect("source file");
        fs::write(source.path().join("generated-state/cache.bin"), "cache\n")
            .expect("excluded state file");
        fs::write(source.path().join("local.state"), "state\n").expect("excluded marker");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}","policy":{{"snapshot_excludes":["generated-state","generated-state/**","*.state"]}}}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");

        assert_eq!(exit_code, 0);
        assert_eq!(output.counts.files, 1);
        assert!(output.excludes.contains(&"generated-state/**".to_string()));
        assert!(Path::new(&output.remote_path)
            .join("src/source.txt")
            .exists());
        assert!(!Path::new(&output.remote_path)
            .join("generated-state/cache.bin")
            .exists());
        assert!(!Path::new(&output.remote_path).join("local.state").exists());
    });
}

#[test]
fn test_sync_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("src")).expect("src dir");
        fs::create_dir_all(source.path().join("build")).expect("root build dir");
        fs::create_dir_all(source.path().join("vendor")).expect("vendor dir");
        fs::create_dir_all(source.path().join("wordpress/scripts/build"))
            .expect("extension scripts build dir");
        fs::create_dir_all(source.path().join(".git")).expect("git dir");
        fs::create_dir_all(source.path().join("target/debug")).expect("target dir");
        fs::create_dir_all(source.path().join("packages/cli")).expect("package dir");
        fs::write(source.path().join("src/main.rs"), "fn main() {}\n").expect("source file");
        fs::write(source.path().join("build/bundle.js"), "artifact").expect("build file");
        fs::write(source.path().join("vendor/autoload.php"), "<?php\n").expect("vendor file");
        fs::write(
            source.path().join("wordpress/scripts/build/setup.sh"),
            "#!/bin/sh\n",
        )
        .expect("extension setup source file");
        fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n")
            .expect("git metadata");
        fs::write(source.path().join("src/._main.rs"), "appledouble").expect("sidecar file");
        fs::write(source.path().join(".env.local"), "SECRET=1\n").expect("secret file");
        fs::write(source.path().join("target/debug/homeboy"), "binary").expect("build file");
        fs::write(
            source.path().join("packages/cli/tsconfig.tsbuildinfo"),
            "stale incremental state",
        )
        .expect("tsbuildinfo file");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");

        assert_eq!(exit_code, 0);
        assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Snapshot);
        assert_eq!(output.current_workspace.local_path, output.local_path);
        assert_eq!(output.current_workspace.remote_path, output.remote_path);
        assert_eq!(
            output.current_workspace.sync_mode,
            RunnerWorkspaceSyncMode::Snapshot
        );
        assert!(output.current_workspace.materialized);
        assert_eq!(output.current_workspace.source_commit, None);
        assert_eq!(output.current_workspace.source_ref, None);
        assert_eq!(output.current_workspace.source_dirty, None);
        assert_eq!(output.counts.files, 4);
        assert!(Path::new(&output.remote_path).join("src/main.rs").exists());
        assert!(Path::new(&output.remote_path)
            .join("vendor/autoload.php")
            .exists());
        assert!(Path::new(&output.remote_path)
            .join("wordpress/scripts/build/setup.sh")
            .exists());
        assert!(!Path::new(&output.remote_path).join(".git").exists());
        assert!(Path::new(&output.remote_path)
            .join("build/bundle.js")
            .exists());
        assert!(!Path::new(&output.remote_path)
            .join("src/._main.rs")
            .exists());
        assert!(!Path::new(&output.remote_path).join(".env.local").exists());
        assert!(!Path::new(&output.remote_path)
            .join("target/debug/homeboy")
            .exists());
        assert!(!Path::new(&output.remote_path)
            .join("packages/cli/tsconfig.tsbuildinfo")
            .exists());
    });
}

#[test]
fn snapshot_sync_uses_unique_clean_workspace_for_same_snapshot() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::write(source.path().join("Cargo.toml"), "[package]\nname='app'\n")
            .expect("manifest");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let options = RunnerWorkspaceSyncOptions {
            path: source.path().display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        };
        let (first, _) = sync_workspace("lab-local", options.clone()).expect("first sync");
        let remote_path = Path::new(&first.remote_path);
        assert!(remote_path.join("Cargo.toml").exists());

        fs::write(remote_path.join("sentinel.txt"), "kept\n").expect("sentinel");

        let (second, _) = sync_workspace("lab-local", options).expect("second sync");
        let second_remote_path = Path::new(&second.remote_path);

        assert_ne!(second.remote_path, first.remote_path);
        assert!(second_remote_path.join("Cargo.toml").exists());
        assert!(!second_remote_path.join("sentinel.txt").exists());
        assert!(remote_path.join("sentinel.txt").exists());
    });
}

#[test]
fn snapshot_install_restores_workspace_owner_after_root_run() {
    let command =
        snapshot_install_command("/var/lib/sampleplugin/workspace/_lab_workspaces/homeboy-abc");

    assert!(command.contains("owner_path=$parent"));
    assert!(command.contains("mkdir -p \"$parent\""));
    assert!(command.contains("mv \"$tmp\" \"$dest\" && if"));
    assert!(command.contains("chown -R \"$owner\" $dest"));
}

#[test]
fn snapshot_archive_command_disables_extended_attributes() {
    let command = snapshot_archive_command(
        Path::new("/Users/user/Developer/wp-site-generator"),
        "ssh runner 'tar -xf -'",
        &[],
    );

    assert!(command.contains("COPYFILE_DISABLE=1"));
    assert!(command.contains("tar --no-xattrs"));
}

#[test]
fn snapshot_archive_command_dereferences_symlinked_dependencies() {
    // Symlinked plan dependencies (e.g. a `.ci/<dep>` link to a sibling
    // checkout) must be materialized into the runner snapshot so offloaded
    // plans whose embedded paths traverse the symlink resolve on the runner
    // instead of dangling (#3913).
    let command = snapshot_archive_command(
        Path::new("/Users/user/Developer/wp-site-generator"),
        "ssh runner 'tar -xf -'",
        &[],
    );

    assert!(
        command.contains("tar --no-xattrs -h "),
        "snapshot tar must dereference symlinks: {command}"
    );
}

#[test]
#[cfg(unix)]
fn copy_snapshot_materializes_symlinked_dependency_contents() {
    // End-to-end guard for #3913: a primary workspace that wires a
    // dependency in via a symlink (here `.ci/dep` -> a sibling checkout)
    // must land the real dependency file contents in the snapshot, not a
    // dangling link, so an offloaded plan path traversing the symlink
    // resolves on the runner.
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("primary");
    let dependency = controller.path().join("dependency");
    let dependency_file = dependency.join("packages/cli/dist/index.js");
    std::fs::create_dir_all(dependency_file.parent().unwrap()).expect("dependency dir");
    std::fs::write(&dependency_file, "#!/usr/bin/env node\n").expect("dependency file");
    std::fs::create_dir_all(source.join(".ci")).expect("ci dir");
    std::os::unix::fs::symlink(&dependency, source.join(".ci/dep")).expect("dep symlink");

    let destination = controller.path().join("snapshot");
    crate::core::runner::workspace::snapshot::copy_snapshot_to_directory(
        &source,
        &destination,
        &[],
    )
    .expect("copy snapshot");

    let materialized = destination.join(".ci/dep/packages/cli/dist/index.js");
    assert!(
        !materialized.symlink_metadata().expect("entry").is_symlink(),
        "symlinked dependency directory must be dereferenced, not copied as a link"
    );
    assert_eq!(
        std::fs::read_to_string(&materialized).expect("materialized dependency file"),
        "#!/usr/bin/env node\n"
    );
}
