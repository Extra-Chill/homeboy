use super::git;

use std::fs;
use std::path::Path;

use crate::core::runner::workspace::snapshot::{
    copy_snapshot_to_directory, snapshot_archive_command, snapshot_install_command,
    workspace_content_hash, workspace_content_hash_for_policy,
    WORKSPACE_CONTENT_PERMISSION_PORTABLE, WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
};
use crate::core::runner::workspace::sync::{list_workspaces, sync_workspace};
use crate::core::runner::workspace::types::{
    RunnerWorkspaceOutputPaths, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::core::runner::workspace::util::git_output;

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
fn runner_snapshot_rejects_source_runner_workspace_metadata_collision() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join(".homeboy")).expect("metadata directory");
        fs::write(
            source.path().join(".homeboy/runner-workspace.json"),
            "user-owned collision\n",
        )
        .expect("metadata collision");
        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-collision","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let error = sync_workspace(
            "lab-local-collision",
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
        .expect_err("reserved runner metadata must reject staging");

        assert!(error.message.contains("reserved runner metadata path"));
        assert!(error.message.contains("remove or rename"));
        assert_eq!(
            fs::read_dir(runner_root.path())
                .expect("runner root entries")
                .count(),
            0,
            "collision must fail before creating a materialized workspace"
        );
    });
}

#[test]
fn generic_snapshot_copy_allows_source_owned_runner_workspace_path() {
    let source = tempfile::tempdir().expect("source tempdir");
    let destination = tempfile::tempdir().expect("destination tempdir");
    fs::create_dir_all(source.path().join(".homeboy")).expect("metadata directory");
    fs::write(
        source.path().join(".homeboy/runner-workspace.json"),
        "source-owned generic snapshot content\n",
    )
    .expect("source metadata");

    copy_snapshot_to_directory(source.path(), destination.path(), &[])
        .expect("generic snapshot copy");

    assert_eq!(
        fs::read_to_string(destination.path().join(".homeboy/runner-workspace.json"))
            .expect("copied metadata"),
        "source-owned generic snapshot content\n"
    );
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
        fs::write(source.path().join(".git/HEAD"), "ref: refs/heads/main\n").expect("git metadata");
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
        assert_eq!(output.counts.files, 6);
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
        assert!(Path::new(&output.remote_path)
            .join("target/debug/homeboy")
            .exists());
        assert!(Path::new(&output.remote_path)
            .join("packages/cli/tsconfig.tsbuildinfo")
            .exists());
    });
}

#[test]
fn snapshot_sync_uses_gitignore_excludes_as_generic_fallback() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(source.path(), &["init"]);
        fs::write(
            source.path().join(".gitignore"),
            "target/\nnode_modules/\n*.tsbuildinfo\n",
        )
        .expect("gitignore");
        fs::create_dir_all(source.path().join("src")).expect("src dir");
        fs::create_dir_all(source.path().join("target/debug")).expect("target dir");
        fs::create_dir_all(source.path().join("node_modules/pkg")).expect("node_modules dir");
        fs::write(source.path().join("src/main.rs"), "fn main() {}\n").expect("source file");
        fs::write(source.path().join("target/debug/homeboy"), "binary").expect("build file");
        fs::write(source.path().join("node_modules/pkg/index.js"), "module").expect("module file");
        fs::write(source.path().join("build.tsbuildinfo"), "state").expect("state file");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-gitignore","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local-gitignore",
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
        assert!(output.excludes.contains(&"target".to_string()));
        assert!(output.excludes.contains(&"node_modules/**".to_string()));
        assert!(output.excludes.contains(&"*.tsbuildinfo".to_string()));
        assert!(Path::new(&output.remote_path).join("src/main.rs").exists());
        assert!(!Path::new(&output.remote_path)
            .join("target/debug/homeboy")
            .exists());
        assert!(!Path::new(&output.remote_path)
            .join("node_modules/pkg/index.js")
            .exists());
        assert!(!Path::new(&output.remote_path)
            .join("build.tsbuildinfo")
            .exists());
    });
}

#[test]
fn snapshot_sync_uses_unique_clean_workspace_for_same_snapshot() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::write(source.path().join("Cargo.toml"), "[package]\nname='app'\n").expect("manifest");

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
fn workspace_sync_materialization_contract_records_inputs_provenance_policy_and_paths() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("src")).expect("src dir");
        fs::write(source.path().join("src/main.rs"), "fn main() {}\n").expect("source file");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-contract","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, _) = sync_workspace(
            "lab-local-contract",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: true,
                changed_since_base: Some("origin/trunk".to_string()),
                git_fetch_refs: vec!["refs/heads/trunk".to_string()],
                snapshot_includes: vec!["src/**".to_string()],
                allow_dirty_lab_workspace: true,
                run_isolation_token: Some("run-123".to_string()),
            },
        )
        .expect("sync workspace");

        let contract = &output.materialization_plan;
        assert_eq!(
            contract.declared_inputs.path,
            source.path().display().to_string()
        );
        assert_eq!(
            contract.declared_inputs.mode,
            RunnerWorkspaceSyncMode::Snapshot
        );
        assert!(contract.declared_inputs.controller_routed_git);
        assert_eq!(
            contract.declared_inputs.changed_since_base.as_deref(),
            Some("origin/trunk")
        );
        assert_eq!(
            contract.declared_inputs.git_fetch_refs,
            vec!["refs/heads/trunk".to_string()]
        );
        assert_eq!(
            contract.declared_inputs.snapshot_includes,
            vec!["src/**".to_string()]
        );
        assert_eq!(contract.source_provenance.local_path, output.local_path);
        assert_eq!(
            contract.source_provenance.identity,
            output.snapshot_identity
        );
        assert_eq!(contract.run_isolation_token.as_deref(), Some("run-123"));
        assert!(contract.dirty_policy.allow_dirty_lab_workspace);
        assert_eq!(
            contract.dirty_policy.workspace_cleanliness,
            output.workspace_cleanliness
        );
        assert_eq!(
            contract.output_paths.workspace_root,
            runner_root.path().display().to_string()
        );
        assert_eq!(contract.output_paths.remote_path, output.remote_path);
        assert_eq!(
            contract.output_paths.lab_workspaces_root,
            format!("{}/_lab_workspaces", runner_root.path().display())
        );
        assert_eq!(
            contract.output_paths.artifact_dir,
            RunnerWorkspaceOutputPaths::artifact_dir_for_workspace(&output.remote_path)
        );
    });
}

#[test]
fn workspace_list_reports_recent_lab_workspaces_with_exec_commands() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::write(source.path().join("Cargo.toml"), "[package]\nname='app'\n").expect("manifest");

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-list","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (sync, _) = sync_workspace(
            "lab-local-list",
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

        let (list, exit_code) = list_workspaces("lab-local-list", 10).expect("list workspaces");

        assert_eq!(exit_code, 0);
        assert_eq!(list.command, "runner.workspace.list");
        assert_eq!(
            list.lab_workspaces_root,
            format!("{}/_lab_workspaces", runner_root.path().display())
        );
        assert_eq!(list.workspaces.len(), 1);
        assert_eq!(list.workspaces[0].remote_path, sync.remote_path);
        assert!(list.workspaces[0]
            .exec_command
            .contains("homeboy runner exec lab-local-list --cwd"));
        assert!(list.workspaces[0].exec_command.contains("-- <command>"));
    });
}

#[test]
fn snapshot_git_sync_materializes_dirty_source_as_synthetic_git_checkout() {
    crate::test_support::with_isolated_home(|_| {
        let source = super::dirty_git_repo();
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(
            source.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/example/app.git",
            ],
        );

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-snapshot-git","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local-snapshot-git",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::SnapshotGit,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");

        let remote = Path::new(&output.remote_path);
        assert_eq!(exit_code, 0);
        assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::SnapshotGit);
        assert_eq!(
            output.current_workspace.sync_mode,
            RunnerWorkspaceSyncMode::SnapshotGit
        );
        assert_eq!(output.current_workspace.source_dirty, Some(true));
        assert_eq!(
            output.workspace_cleanliness,
            "snapshot_synthetic_git_unique_workspace"
        );
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(
            git_output(remote, &["rev-parse", "--is-inside-work-tree"]).unwrap(),
            "true"
        );
        assert_eq!(
            git_output(remote, &["config", "--get", "remote.origin.url"]).unwrap(),
            "https://github.com/example/app.git"
        );
        assert_eq!(
            git_output(remote, &["status", "--porcelain=v1"]).unwrap(),
            "",
            "Homeboy-owned runner metadata must not dirty synthetic checkouts before patch capture"
        );
        assert!(fs::read_to_string(remote.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .any(|line| line == ".homeboy/"));
        assert!(git_output(remote, &["log", "-1", "--pretty=%B"])
            .unwrap()
            .contains(&output.snapshot_identity));

        // The synthetic checkout identity must be surfaced as run evidence so a
        // write-capable agent-task dispatch can trace the dirty controller-side
        // worktree back to the synthetic commit carrying it into the runner
        // workspace (#6136 acceptance criterion #2).
        let synthetic_commit = output
            .current_workspace
            .synthetic_checkout_commit
            .clone()
            .expect("synthetic checkout commit recorded as run evidence");
        assert_eq!(
            synthetic_commit,
            git_output(remote, &["rev-parse", "HEAD"]).unwrap(),
            "recorded synthetic checkout commit must match the materialized workspace HEAD"
        );
        assert!(
            output.current_workspace.source_commit.is_some(),
            "snapshot-git evidence must also record the source commit"
        );
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
fn snapshot_content_hash_matches_materialized_workspace_after_runner_metadata_injection() {
    // This mirrors a Lab snapshot of a repository such as homeboy-extensions:
    // the runner creates its metadata directory after extracting a source that
    // has no `.homeboy` directory of its own.
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("homeboy-extensions@fixture");
    let dependency = controller.path().join("dependency");
    let destination = controller.path().join("materialized");
    let excludes = vec![
        ".git/".to_string(),
        "generated-state".to_string(),
        "generated-state/**".to_string(),
    ];

    fs::create_dir_all(source.join("packages/runtime")).expect("source package directory");
    fs::create_dir_all(source.join("generated-state")).expect("generated state directory");
    fs::write(
        source.join("packages/runtime/runner.sh"),
        "#!/bin/sh\nexit 0\n",
    )
    .expect("runner script");
    fs::write(source.join("generated-state/cache.bin"), "excluded\n").expect("generated state");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let script = source.join("packages/runtime/runner.sh");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("executable mode");
        fs::create_dir_all(dependency.join("dist")).expect("dependency directory");
        fs::write(dependency.join("dist/index.js"), "export default {};\n")
            .expect("dependency file");
        symlink(&dependency, source.join("packages/runtime/dependency"))
            .expect("dependency symlink");
    }

    let expected = workspace_content_hash(&source, &excludes).expect("controller hash");
    copy_snapshot_to_directory(&source, &destination, &excludes).expect("materialize snapshot");
    fs::create_dir_all(destination.join(".homeboy")).expect("runner metadata directory");
    fs::write(
        destination.join(".homeboy/runner-workspace.json"),
        r#"{"schema":"homeboy/runner-workspace/v1"}"#,
    )
    .expect("runner metadata");

    assert!(destination.join("packages/runtime/runner.sh").is_file());
    assert!(!destination.join("generated-state").exists());
    assert_eq!(
        workspace_content_hash(&destination, &excludes).expect("materialized hash"),
        expected,
        "the controller hash must describe the bytes and structure that the runner verifies"
    );
}

#[test]
#[cfg(unix)]
fn workspace_content_hash_normalizes_git_materialization_umask_modes() {
    use std::os::unix::fs::PermissionsExt;

    let controller = tempfile::tempdir().expect("controller");
    let runner = tempfile::tempdir().expect("runner");
    for root in [controller.path(), runner.path()] {
        fs::create_dir_all(root.join("src")).expect("source directory");
        fs::write(root.join("src/library.rs"), "pub fn fixture() {}\n").expect("source file");
        fs::write(root.join("src/run.sh"), "#!/bin/sh\n").expect("executable file");
    }
    fs::set_permissions(
        controller.path().join("src"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("controller directory mode");
    fs::set_permissions(runner.path().join("src"), fs::Permissions::from_mode(0o775))
        .expect("runner directory mode");
    fs::set_permissions(
        controller.path().join("src/library.rs"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("controller file mode");
    fs::set_permissions(
        runner.path().join("src/library.rs"),
        fs::Permissions::from_mode(0o664),
    )
    .expect("runner file mode");
    fs::set_permissions(
        controller.path().join("src/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("controller executable mode");
    fs::set_permissions(
        runner.path().join("src/run.sh"),
        fs::Permissions::from_mode(0o775),
    )
    .expect("runner executable mode");

    assert_eq!(
        workspace_content_hash(controller.path(), &[]).expect("controller hash"),
        workspace_content_hash(runner.path(), &[]).expect("runner hash"),
        "Git checkout umask differences must not change materialized content identity"
    );
}

#[test]
#[cfg(unix)]
fn workspace_content_hash_portable_policy_is_platform_mode_independent() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempfile::tempdir().expect("workspace");
    let file = workspace.path().join("file.txt");
    fs::write(&file, "portable bytes\n").expect("source file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("non-executable mode");
    let non_executable = workspace_content_hash_for_policy(
        workspace.path(),
        &[],
        WORKSPACE_CONTENT_PERMISSION_PORTABLE,
    )
    .expect("non-executable hash");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("executable mode");
    assert_eq!(
        workspace_content_hash_for_policy(
            workspace.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_PORTABLE,
        )
        .expect("executable hash"),
        non_executable,
        "portable v2 identity must not bind platform-specific permission bits"
    );
}

#[test]
#[cfg(unix)]
fn workspace_content_hash_unix_policy_binds_executable_bit_without_umask_bits() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempfile::tempdir().expect("workspace");
    let file = workspace.path().join("file.txt");
    fs::write(&file, "portable bytes\n").expect("source file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("non-executable mode");
    let non_executable = workspace_content_hash_for_policy(
        workspace.path(),
        &[],
        WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    )
    .expect("non-executable hash");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o664)).expect("umask variant");
    assert_eq!(
        workspace_content_hash_for_policy(
            workspace.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
        )
        .expect("umask variant hash"),
        non_executable
    );
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).expect("executable mode");
    assert_ne!(
        workspace_content_hash_for_policy(
            workspace.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
        )
        .expect("executable hash"),
        non_executable
    );
}

#[test]
#[cfg(not(unix))]
fn workspace_content_hash_non_unix_rejects_unix_executable_policy() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(workspace.path().join("file.txt"), "portable bytes\n").expect("source file");
    let error = workspace_content_hash_for_policy(
        workspace.path(),
        &[],
        WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    )
    .expect_err("non-Unix cannot verify Unix executable policy");
    assert!(error.message.contains("unsupported on this platform"));
}

#[test]
fn snapshot_content_hash_binds_user_owned_homeboy_files_but_ignores_runner_metadata() {
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("homeboy-extensions@fixture");
    let destination = controller.path().join("materialized");
    let excludes = vec![".git/".to_string()];
    fs::create_dir_all(source.join(".homeboy")).expect("user metadata directory");
    fs::create_dir_all(source.join("src")).expect("source directory");
    fs::write(
        source.join(".homeboy/user-settings.json"),
        "{\"enabled\":true}\n",
    )
    .expect("user metadata");
    fs::write(source.join("src/lib.rs"), "pub fn fixture() {}\n").expect("source file");

    let expected = workspace_content_hash(&source, &excludes).expect("controller hash");
    copy_snapshot_to_directory(&source, &destination, &excludes).expect("materialize snapshot");
    fs::write(
        destination.join(".homeboy/runner-workspace.json"),
        r#"{"schema":"homeboy/runner-workspace/v1"}"#,
    )
    .expect("runner metadata");

    assert_eq!(
        workspace_content_hash(&destination, &excludes).expect("materialized hash"),
        expected,
        "runner metadata must not change the controller identity"
    );

    fs::write(
        destination.join(".homeboy/runner-workspace.json"),
        r#"{"schema":"homeboy/runner-workspace/v2","changed":true}"#,
    )
    .expect("changed runner metadata");
    assert_eq!(
        workspace_content_hash(&destination, &excludes).expect("metadata-insensitive hash"),
        expected,
        "runner metadata bytes and mode are transport state"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            destination.join(".homeboy/runner-workspace.json"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("changed runner metadata mode");
        assert_eq!(
            workspace_content_hash(&destination, &excludes).expect("mode-insensitive hash"),
            expected,
            "runner metadata mode is transport state"
        );
    }

    fs::write(
        destination.join(".homeboy/user-settings.json"),
        "{\"enabled\":false}\n",
    )
    .expect("changed user metadata");
    assert_ne!(
        workspace_content_hash(&destination, &excludes).expect("mutated hash"),
        expected,
        "user-owned `.homeboy` children must remain fail-closed"
    );

    fs::write(
        destination.join(".homeboy/user-settings.json"),
        "{\"enabled\":true}\n",
    )
    .expect("restore user metadata");
    assert_eq!(
        workspace_content_hash(&destination, &excludes).expect("restored hash"),
        expected,
        "restoring user metadata restores the materialized identity"
    );
    fs::write(destination.join("src/lib.rs"), "pub fn changed() {}\n")
        .expect("changed source file");
    assert_ne!(
        workspace_content_hash(&destination, &excludes).expect("changed source hash"),
        expected,
        "ordinary workspace files must remain bound by the identity"
    );
}

#[test]
fn snapshot_content_hash_matches_tar_when_homeboy_is_excluded() {
    for pattern in [".homeboy", ".homeboy/", ".homeboy/**"] {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("source");
        let destination = controller.path().join("materialized");
        let excludes = vec![pattern.to_string()];
        fs::create_dir_all(source.join(".homeboy")).expect("user metadata directory");
        fs::create_dir_all(source.join("src")).expect("source directory");
        fs::write(source.join(".homeboy/user-settings.json"), "user-owned\n")
            .expect("user metadata");
        fs::write(source.join("src/lib.rs"), "pub fn fixture() {}\n").expect("source file");

        let expected = workspace_content_hash(&source, &excludes).expect("controller hash");
        copy_snapshot_to_directory(&source, &destination, &excludes).expect("materialize snapshot");
        fs::create_dir_all(destination.join(".homeboy")).expect("runner metadata directory");
        fs::write(
            destination.join(".homeboy/runner-workspace.json"),
            "transport metadata\n",
        )
        .expect("runner metadata");

        assert_eq!(
            workspace_content_hash(&destination, &excludes).expect("materialized hash"),
            expected,
            "exclude pattern `{pattern}` must hash the same tree tar materializes"
        );
    }
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
