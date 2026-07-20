use super::git;

use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::workspace::snapshot::{
    copy_snapshot_to_directory, ensure_no_runner_workspace_metadata_collision,
    snapshot_archive_command, snapshot_install_command, synthetic_checkout_value,
    workspace_content_hash, workspace_content_hash_algorithm, workspace_content_hash_for_policy,
    workspace_content_hash_v1, workspace_content_manifest_for_policy,
    WORKSPACE_CONTENT_PERMISSION_PORTABLE, WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
};

#[test]
fn snapshot_git_readback_failure_fails_materialization_contract() {
    let runner: crate::Runner = serde_json::from_value(serde_json::json!({
        "id": "lab", "kind": "local"
    }))
    .expect("local runner");

    let error = synthetic_checkout_value(&runner, "/does-not-exist", "rev-parse HEAD")
        .expect_err("missing checkout readback must fail");

    assert!(format!("{error:?}")
        .contains("could not read `rev-parse HEAD` from synthetic snapshot-git checkout"));
}

#[test]
fn snapshot_git_readback_failure_rolls_back_remote_workspace_and_registration() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        fs::write(source.path().join("file.txt"), "snapshot\n").expect("source file");
        let runner_root = tempfile::tempdir().expect("runner root");
        let shim_root = tempfile::tempdir().expect("git shim root");
        let shim = shim_root.path().join("git");
        fs::write(
            &shim,
            "#!/bin/sh\nif [ \"$1\" = \"-C\" ] && [ \"$3\" = \"rev-parse\" ] && [ \"$4\" = \"HEAD\" ] && [ -e \"$2/.git/refs/notes/homeboy-snapshot\" ]; then exit 1; fi\nexec /usr/bin/git \"$@\"\n",
        )
        .expect("write git shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
                .expect("make git shim executable");
        }
        crate::create(
            &format!(
                r#"{{"id":"lab-readback-failure","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let original_path = std::env::var_os("PATH").expect("PATH");
        let mut path_entries = vec![shim_root.path().display().to_string()];
        path_entries
            .extend(std::env::split_paths(&original_path).map(|path| path.display().to_string()));
        std::env::set_var("PATH", path_entries.join(":"));
        let result = sync_workspace(
            "lab-readback-failure",
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
        );
        std::env::set_var("PATH", original_path);

        let error = result.expect_err("required snapshot-git readback must fail");
        assert!(format!("{error:?}").contains("synthetic snapshot-git checkout"));
        let workspaces_root = runner_root.path().join("_lab_workspaces");
        assert!(
            !workspaces_root.exists()
                || fs::read_dir(&workspaces_root)
                    .expect("read workspaces root")
                    .next()
                    .is_none(),
            "readback failure must remove the materialized remote workspace"
        );
        let (listed, exit_code) =
            list_workspaces("lab-readback-failure", 10).expect("list workspaces");
        assert_eq!(exit_code, 0);
        assert!(
            listed.workspaces.is_empty(),
            "readback failure must not register a workspace"
        );
    });
}
use crate::workspace::sync::{list_workspaces, sync_workspace};
use crate::workspace::types::{
    RunnerWorkspaceOutputPaths, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::workspace::util::git_output;
use homeboy_core::source_snapshot::SourceSnapshot;

static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
fn snapshot_git_reports_checkout_provenance_for_committed_harvest() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        let runner_root = tempfile::tempdir().expect("runner root");
        fs::write(source.path().join("file.txt"), "committed source\n").expect("source file");
        std::process::Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(source.path())
            .status()
            .expect("initialize source repository");
        std::process::Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(source.path())
            .status()
            .expect("stage source");
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "--quiet",
                "-m",
                "source",
            ])
            .current_dir(source.path())
            .status()
            .expect("commit source");
        let source_revision =
            git_output(source.path(), &["rev-parse", "HEAD"]).expect("source SHA");
        crate::create(
            &format!(
                r#"{{"id":"lab-snapshot-harvest","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (synced, _) = sync_workspace(
            "lab-snapshot-harvest",
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
        .expect("materialize local-only committed source");

        assert_eq!(synced.sync_mode, RunnerWorkspaceSyncMode::SnapshotGit);
        assert_eq!(
            synced
                .materialization_plan
                .actual_materialization_mode
                .as_deref(),
            Some(RunnerWorkspaceSyncMode::SnapshotGit.label())
        );
        assert_eq!(
            git_output(Path::new(&synced.remote_path), &["rev-parse", "HEAD"])
                .expect("materialized SHA"),
            source_revision
        );
        let mut snapshot = homeboy_core::source_snapshot::collect_local(
            "lab-snapshot-harvest",
            source.path(),
            Some(&synced.remote_path),
            "lab_offload",
        );
        snapshot.sync_excludes = synced.excludes.clone();
        snapshot.workspace_snapshot_identity = Some(synced.snapshot_identity.clone());
        let content_hash = workspace_content_hash(source.path(), &snapshot.sync_excludes)
            .expect("source content hash");
        let snapshot_json = serde_json::to_value(&snapshot).expect("snapshot JSON");
        let lab = serde_json::json!({
            "runner_id": "lab-snapshot-harvest",
            "remote_workspace": synced.remote_path.clone(),
            "sync_mode": synced.materialization_plan.actual_materialization_mode,
            "status": "offloaded",
            "source_snapshot": snapshot_json,
            "workspace_cleanliness": { "allow_dirty_lab_workspace": false },
            "workspace_verification": {
                "schema": "homeboy/lab-workspace-verification/v2",
                "identity": synced.snapshot_identity.clone(),
                "content_hash_algorithm": workspace_content_hash_algorithm(
                    super::super::snapshot::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
                ).expect("content hash algorithm"),
                "permission_policy": super::super::snapshot::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
                "content_hash": content_hash,
                "sync_excludes": snapshot.sync_excludes,
                "source_snapshot": snapshot.clone(),
                "primary_workspace": {
                    "identity": synced.snapshot_identity.clone(),
                    "remote_path": synced.remote_path.clone(),
                },
            },
        });
        let provenance = super::super::provenance::verify_lab_workspace(
            &synced.remote_path,
            Path::new(&synced.remote_path),
            snapshot,
            lab.clone(),
        )
        .expect("committed-harvest provenance");
        super::super::provenance::verify_lab_workspace_git_root(
            Path::new(&synced.remote_path),
            &provenance,
        )
        .expect("committed-harvest Git root");

        // Agent-task @plan staging is runner-owned execution state, not a
        // source change. It can be staged by a runtime, so exercise the exact
        // committed-harvest verifier with that state present.
        let at_file =
            Path::new(&synced.remote_path).join(".homeboy/lab-at-files/agent-task-plan.json");
        fs::create_dir_all(at_file.parent().expect("@file parent")).expect("create @file parent");
        fs::write(&at_file, "{}\n").expect("write staged agent-task plan");
        std::process::Command::new("git")
            .args([
                "add",
                "--force",
                ".homeboy/lab-at-files/agent-task-plan.json",
            ])
            .current_dir(&synced.remote_path)
            .status()
            .expect("stage runner-owned plan");
        super::super::provenance::verify_lab_workspace_git_root(
            Path::new(&synced.remote_path),
            &provenance,
        )
        .expect("runner-owned agent-task plan does not dirty the verified snapshot");

        fs::write(
            Path::new(&synced.remote_path).join("unexpected.txt"),
            "unexpected\n",
        )
        .expect("write unexpected source change");
        let error = super::super::provenance::verify_lab_workspace_git_root(
            Path::new(&synced.remote_path),
            &provenance,
        )
        .expect_err("unexpected source change remains dirty");
        assert!(
            error.contains("content hash") || error.contains("cleanliness does not match"),
            "unexpected source change must remain bound to provenance: {error}"
        );
    });
}

#[test]
fn snapshot_command_failure_keeps_exit_status_and_silent_transport_cause() {
    let error =
        super::super::util::run_shell_command("exit 23", "materialize SSH workspace snapshot")
            .expect_err("silent command failure must be actionable");

    assert_eq!(
        error.message,
        "materialize SSH workspace snapshot failed during command execution (exit status 23): the command exited without stdout or stderr"
    );
}

#[test]
fn snapshot_command_failure_preserves_bounded_transport_output() {
    let error = super::super::util::run_shell_command(
        "printf stdout-evidence; printf stderr-evidence >&2; exit 24",
        "materialize SSH workspace snapshot",
    )
    .expect_err("transport output must be retained");

    assert!(error.message.contains("exit status 24"));
    assert!(error.message.contains("stdout: stdout-evidence"));
    assert!(error.message.contains("stderr: stderr-evidence"));
}

#[test]
fn snapshot_command_failure_bounds_transport_output() {
    let error = super::super::util::run_shell_command(
        "head -c 5000 /dev/zero | tr '\\0' x; exit 25",
        "materialize SSH workspace snapshot",
    )
    .expect_err("large transport output must be bounded");

    assert!(error.message.contains("exit status 25"));
    assert!(error.message.ends_with("... [truncated]"));
    assert!(error.message.len() < 4_300);
}

#[test]
fn snapshot_signal_death_is_a_retryable_transport_failure() {
    // #8803: an SSH transport that drops mid-pipe kills `sh` with a signal, so
    // it exits with no code (surfaced as -1). This must be classified as a
    // retryable transport failure carrying structured diagnostics, not an
    // opaque internal error.
    let error = super::super::util::run_shell_command(
        "kill -PIPE $$",
        "materialize SSH workspace snapshot",
    )
    .expect_err("signal death must be an actionable transport failure");

    assert_eq!(
        error.code,
        homeboy_core::error::ErrorCode::RunnerLabTransportFailure
    );
    assert_eq!(
        error.retryable,
        Some(true),
        "transport failures must be retryable"
    );
    let details = serde_json::to_string(&error.details).expect("serialize details");
    assert!(
        details.contains("\"signal_death\":true"),
        "must record that the process was killed by a signal: {details}"
    );
    assert!(
        details.contains("transport_close_reason"),
        "must record a transport close reason: {details}"
    );
    // The generic non-transport message must not be used for a transport drop.
    assert!(
        !error.message.contains("failed during command execution"),
        "signal death must not fall through to the generic command error: {}",
        error.message
    );
}

#[test]
fn snapshot_ssh_connection_exit_is_a_retryable_transport_failure() {
    // SSH exits 255 on a connection-level error, distinct from a remote
    // command's own non-zero exit code.
    let error = super::super::util::run_shell_command(
        "echo 'ssh: connect to host lab port 22: Connection refused' >&2; exit 255",
        "materialize SSH workspace snapshot",
    )
    .expect_err("ssh connection error must be an actionable transport failure");

    assert_eq!(
        error.code,
        homeboy_core::error::ErrorCode::RunnerLabTransportFailure
    );
    assert_eq!(error.retryable, Some(true));
    assert!(
        error.message.to_lowercase().contains("connection refused"),
        "must surface the transport close reason: {}",
        error.message
    );
}

#[test]
fn snapshot_ordinary_command_failure_is_not_classified_as_transport() {
    // A genuine remote command failure (non-signal, non-255, no transient
    // stderr) must remain a plain command error so real bugs are not silently
    // retried as transport flakes.
    let error = super::super::util::run_shell_command(
        "echo boom >&2; exit 2",
        "materialize SSH workspace snapshot",
    )
    .expect_err("ordinary failure still errors");

    assert_ne!(
        error.code,
        homeboy_core::error::ErrorCode::RunnerLabTransportFailure
    );
    assert!(error.message.contains("exit status 2"));
}

#[test]
fn snapshot_command_failure_bounds_multibyte_output_at_a_character_boundary() {
    let error = super::super::util::run_shell_command(
        "head -c 4095 /dev/zero | tr '\\0' Z; printf '\\342\\202\\254'; exit 26",
        "materialize SSH workspace snapshot",
    )
    .expect_err("multibyte transport output must not panic while truncating");

    assert!(error.message.contains("exit status 26"));
    assert_eq!(error.message.matches('Z').count(), 4095);
    assert!(!error.message.contains('\u{20ac}'));
    assert!(error.message.ends_with("... [truncated]"));
}

#[test]
fn runner_snapshot_includes_override_generated_output_excludes() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("packages/cli/dist")).expect("dist dir");
        fs::write(
            source.path().join("packages/cli/dist/homeboy.js"),
            "built\n",
        )
        .expect("built output");

        crate::create(
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
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("src")).expect("src dir");
        fs::create_dir_all(source.path().join("generated-state")).expect("state dir");
        fs::write(source.path().join("src/source.txt"), "source\n").expect("source file");
        fs::write(source.path().join("generated-state/cache.bin"), "cache\n")
            .expect("excluded state file");
        fs::write(source.path().join("local.state"), "state\n").expect("excluded marker");

        crate::create(
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
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join(".homeboy")).expect("metadata directory");
        fs::write(
            source.path().join(".homeboy/runner-workspace.json"),
            "user-owned collision\n",
        )
        .expect("metadata collision");
        crate::create(
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

        assert!(error.message.contains("reserved runner path"));
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
fn runner_snapshot_rejects_source_lab_at_file_collision() {
    let source = tempfile::tempdir().expect("source tempdir");
    fs::create_dir_all(source.path().join(".homeboy/lab-at-files")).expect("Lab input collision");

    let error = ensure_no_runner_workspace_metadata_collision(source.path())
        .expect_err("reserved Lab input path must reject staging");

    assert!(error.message.contains("reserved runner path"));
    assert!(error.message.contains(".homeboy/lab-at-files"));
    assert!(error.message.contains("remove or rename"));
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
    homeboy_core::test_support::with_isolated_home(|_| {
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

        crate::create(
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
    homeboy_core::test_support::with_isolated_home(|_| {
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

        crate::create(
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
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::write(source.path().join("Cargo.toml"), "[package]\nname='app'\n").expect("manifest");

        crate::create(
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
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::create_dir_all(source.path().join("src")).expect("src dir");
        fs::write(source.path().join("src/main.rs"), "fn main() {}\n").expect("source file");

        crate::create(
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
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        fs::write(source.path().join("Cargo.toml"), "[package]\nname='app'\n").expect("manifest");

        crate::create(
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
fn snapshot_git_sync_falls_back_for_unpublished_commit_and_preserves_dirty_overlay() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = super::dirty_git_repo();
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        let head = git_output(source.path(), &["rev-parse", "HEAD"]).expect("source head");
        fs::write(source.path().join("untracked.txt"), "untracked\n").expect("untracked file");
        git(
            source.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "file:///does-not-exist/unpublished-fixture.git",
            ],
        );

        crate::create(
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
        assert_eq!(git_output(remote, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(
            git_output(remote, &["config", "--get", "remote.origin.url"]).unwrap(),
            "file:///does-not-exist/unpublished-fixture.git"
        );
        let status = git_output(remote, &["status", "--porcelain=v1"]).unwrap();
        assert!(status.contains("file.txt"));
        assert!(status.contains("?? untracked.txt"));
        assert!(fs::read_to_string(remote.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .any(|line| line == ".homeboy/"));
        assert_eq!(
            output.current_workspace.source_commit.as_deref(),
            Some(head.as_str())
        );
        assert_eq!(
            output
                .materialization_plan
                .controller_git_bundle
                .as_ref()
                .expect("git-backed snapshot records controller bundle provenance")
                .source_sha,
            head
        );

        // Lifecycle scripts can clean the checkout before dependency install.
        git(remote, &["reset", "--hard", "HEAD"]);
        git(remote, &["clean", "-ffdqx"]);
        assert_eq!(git_output(remote, &["rev-parse", "HEAD"]).unwrap(), head);
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).unwrap(),
            "base\n",
            "Git cleanup must restore the captured baseline"
        );
        assert!(!remote.join("untracked.txt").exists());
    });
}

#[test]
fn filesystem_snapshot_of_dirty_partial_clone_avoids_git_bundle_closure() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    homeboy_core::test_support::with_isolated_home(|_| {
        let origin = tempfile::tempdir().expect("origin tempdir");
        let author = tempfile::tempdir().expect("author tempdir");
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(origin.path(), &["init", "--bare"]);
        git(origin.path(), &["config", "uploadpack.allowFilter", "true"]);
        git(author.path(), &["init", "-b", "main"]);
        git(author.path(), &["config", "user.email", "test@example.com"]);
        git(author.path(), &["config", "user.name", "Test User"]);
        fs::write(author.path().join("removed.txt"), "base-only\n").expect("base file");
        git(author.path(), &["add", "."]);
        git(author.path(), &["commit", "-m", "base"]);
        let base_blob =
            git_output(author.path(), &["rev-parse", "HEAD:removed.txt"]).expect("base blob");
        fs::remove_file(author.path().join("removed.txt")).expect("remove base file");
        fs::write(author.path().join("file.txt"), "head\n").expect("head file");
        git(author.path(), &["add", "."]);
        git(author.path(), &["commit", "-m", "head"]);
        let head = git_output(author.path(), &["rev-parse", "HEAD"]).expect("head");
        git(
            author.path(),
            &[
                "remote",
                "add",
                "origin",
                &format!("file://{}", origin.path().display()),
            ],
        );
        git(author.path(), &["push", "origin", "main"]);
        git(author.path(), &["checkout", "-b", "unrelated"]);
        fs::write(author.path().join("unrelated.txt"), "unrelated\n").expect("unrelated file");
        git(author.path(), &["add", "."]);
        git(author.path(), &["commit", "-m", "unrelated"]);
        git(author.path(), &["push", "origin", "unrelated"]);
        git(author.path(), &["checkout", "main"]);
        git(
            source.path(),
            &[
                "clone",
                "--filter=blob:none",
                &format!("file://{}", origin.path().display()),
                ".",
            ],
        );
        let source_branch = git_output(source.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
            .expect("source branch");
        assert!(
            git_output(
                source.path(),
                &["rev-list", "--objects", "--missing=print", "HEAD"]
            )
            .expect("inspect partial clone")
            .contains(&format!("?{base_blob}")),
            "fixture must retain a missing historical promisor blob"
        );
        fs::write(source.path().join("file.txt"), "dirty\n").expect("tracked overlay");
        fs::write(source.path().join("untracked.txt"), "untracked\n").expect("untracked overlay");
        fs::write(source.path().join(".env"), "secret\n").expect("excluded secret");
        crate::create(
            &format!(
                r#"{{"id":"lab-local-partial-snapshot","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let shim_root = tempfile::tempdir().expect("git shim root");
        let shim = shim_root.path().join("git");
        fs::write(
            &shim,
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = \"rev-list\" ] && { echo 'git bundle closure enumeration invoked' >&2; exit 91; }; done\nexec /usr/bin/git \"$@\"\n",
        )
        .expect("write git shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
                .expect("make shim executable");
        }
        let original_path = std::env::var_os("PATH").expect("PATH");
        let mut paths = vec![shim_root.path().to_path_buf()];
        paths.extend(std::env::split_paths(&original_path));
        std::env::set_var("PATH", std::env::join_paths(paths).expect("PATH value"));

        let (output, exit_code) = sync_workspace(
            "lab-local-partial-snapshot",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                ..Default::default()
            },
        )
        .expect("filesystem snapshot must not enumerate a Git bundle closure");
        std::env::set_var("PATH", original_path);

        let remote = Path::new(&output.remote_path);
        assert_eq!(exit_code, 0);
        assert!(output.materialization_plan.controller_git_bundle.is_none());
        assert_eq!(
            output
                .materialization_plan
                .actual_materialization_mode
                .as_deref(),
            Some("filesystem_snapshot")
        );
        assert!(!remote.join(".git").exists());
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(
            fs::read_to_string(remote.join("untracked.txt")).unwrap(),
            "untracked\n"
        );
        assert!(!remote.join(".env").exists(), "default exclusions apply");
        assert_eq!(
            output.current_workspace.source_commit.as_deref(),
            Some(head.as_str())
        );
        assert_eq!(
            output.current_workspace.source_ref.as_deref(),
            Some(source_branch.as_str())
        );
        let metadata: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(remote.join(".homeboy/runner-workspace.json"))
                .expect("read workspace metadata"),
        )
        .expect("parse workspace metadata");
        assert_eq!(
            metadata["actual_materialization_mode"],
            "filesystem_snapshot"
        );
        assert_eq!(
            metadata["source_remote_url"],
            format!("file://{}", origin.path().display())
        );
        assert!(
            git_output(
                source.path(),
                &["rev-list", "--objects", "--missing=print", "HEAD"]
            )
            .expect("inspect controller after runner checkout")
            .contains(&format!("?{base_blob}")),
            "runner-side materialization must not hydrate controller promisor objects"
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
fn snapshot_archive_command_selectively_dereferences_external_symlinked_dependencies() {
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
        command.contains("find \"$stage/source\" -type l -exec sh -c"),
        "snapshot archive must inspect symlink targets: {command}"
    );
    assert!(command.contains("tar --no-xattrs -h -C \"$root\""));
    assert!(
        !command.contains("cp -a . \"$stage/source\""),
        "the staging tree must be populated by the exclusion-filtered tar stream: {command}"
    );
    assert!(
        command.contains("tar --no-xattrs -C \"$stage/source\""),
        "the final snapshot archive must preserve internal symlink entries: {command}"
    );
}

#[cfg(unix)]
#[test]
fn git_backed_snapshot_preserves_tracked_internal_file_and_directory_links() {
    use std::os::unix::fs::symlink;

    homeboy_core::test_support::with_isolated_home(|_| {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("source");
        let runner_root = tempfile::tempdir().expect("runner root");
        fs::create_dir_all(source.join("shared")).expect("shared directory");
        fs::create_dir_all(source.join("links")).expect("links directory");
        fs::write(source.join("shared/helper.mjs"), "export default 1;\n").expect("helper");
        fs::write(source.join("shared/tool.mjs"), "export default 2;\n").expect("tool");
        symlink("../shared/helper.mjs", source.join("links/helper.mjs"))
            .expect("internal file link");
        symlink("../shared", source.join("links/shared")).expect("internal directory link");
        git(&source, &["init", "-b", "main"]);
        git(&source, &["add", "."]);
        git(
            &source,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "-m",
                "source",
            ],
        );

        crate::create(
            &format!(
                r#"{{"id":"lab-internal-links","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let (output, _) = sync_workspace(
            "lab-internal-links",
            RunnerWorkspaceSyncOptions {
                path: source.display().to_string(),
                mode: RunnerWorkspaceSyncMode::SnapshotGit,
                ..Default::default()
            },
        )
        .expect("materialize Git-backed snapshot");
        let remote = Path::new(&output.remote_path);

        assert!(remote
            .join("links/helper.mjs")
            .symlink_metadata()
            .expect("file link metadata")
            .file_type()
            .is_symlink());
        assert!(remote
            .join("links/shared")
            .symlink_metadata()
            .expect("directory link metadata")
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(remote.join("links/helper.mjs")).expect("file link target"),
            Path::new("../shared/helper.mjs")
        );
        assert_eq!(
            fs::read_link(remote.join("links/shared")).expect("directory link target"),
            Path::new("../shared")
        );
        assert!(
            git_output(remote, &["status", "--porcelain=v1"])
                .expect("remote status")
                .is_empty(),
            "tracked internal links must not change the exact Git checkout"
        );
    });
}

#[test]
fn root_anchored_dist_exclusion_matches_tar_and_preserves_nested_dist() {
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("source");
    let destination = controller.path().join("materialized");
    let excludes = vec!["./dist".to_string()];
    fs::create_dir_all(source.join("dist")).expect("root dist directory");
    fs::create_dir_all(source.join("packages/example/dist")).expect("nested dist directory");
    fs::write(source.join("dist/output.a"), "root output").expect("root dist output");
    fs::write(source.join("packages/example/dist/input.a"), "nested input")
        .expect("nested dist input");

    let command = snapshot_archive_command(&source, "tar -xf -", &excludes);
    assert!(
        command.contains("find . -mindepth 1 -maxdepth 1 ! -path ./dist -print0"),
        "root-anchored tar input must omit the matching root path: {command}"
    );

    let expected = workspace_content_hash(&source, &excludes).expect("source hash");
    copy_snapshot_to_directory(&source, &destination, &excludes).expect("materialize snapshot");

    assert!(
        !destination.join("dist").exists(),
        "root dist directory must be excluded"
    );
    assert_eq!(
        fs::read_to_string(destination.join("packages/example/dist/input.a"))
            .expect("nested dist input survives"),
        "nested input"
    );
    assert_eq!(
        workspace_content_hash(&destination, &excludes).expect("materialized hash"),
        expected,
        "content hashing must use the same root-anchored exclusion semantics as tar"
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
fn every_content_hash_algorithm_ignores_all_reserved_runner_workspace_paths() {
    // Both the v1 and v2 content-hash traversals must exclude every
    // runner-owned materialization artifact from `RESERVED_RUNNER_WORKSPACE_PATHS`
    // identically. Regression guard for the drift where the v2 traversal was
    // taught to skip `.homeboy/lab-at-files` (#9003) but the v1 traversal was
    // not, so a v1 workspace carrying that runner path would hash differently on
    // the runner than on the controller.
    let controller = tempfile::tempdir().expect("controller");
    let source = controller.path().join("source");
    fs::create_dir_all(source.join("packages")).expect("source package directory");
    fs::write(source.join("packages/app.rs"), "fn main() {}\n").expect("source file");
    let excludes: Vec<String> = Vec::new();

    let expected_v1 = workspace_content_hash_v1(&source, &excludes).expect("v1 source hash");
    let expected_v2 = workspace_content_hash(&source, &excludes).expect("v2 source hash");

    // Inject every reserved runner-owned path, exactly as the runner would after
    // transport, then re-hash. The identity must be unchanged for both
    // algorithms.
    fs::create_dir_all(source.join(".homeboy/lab-at-files")).expect("lab-at-files directory");
    fs::write(
        source.join(".homeboy/lab-at-files/at-input.txt"),
        "runner-owned transport artifact\n",
    )
    .expect("lab-at-files entry");
    fs::write(
        source.join(".homeboy/runner-workspace.json"),
        r#"{"schema":"homeboy/runner-workspace/v1"}"#,
    )
    .expect("runner metadata");

    assert_eq!(
        workspace_content_hash_v1(&source, &excludes).expect("v1 injected hash"),
        expected_v1,
        "v1 content hash must ignore every reserved runner-owned workspace path"
    );
    assert_eq!(
        workspace_content_hash(&source, &excludes).expect("v2 injected hash"),
        expected_v2,
        "v2 content hash must ignore every reserved runner-owned workspace path"
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
#[cfg(unix)]
fn workspace_content_hash_owner_executable_policy_normalizes_non_owner_execute_bits() {
    use std::os::unix::fs::PermissionsExt;

    let controller = tempfile::tempdir().expect("controller workspace");
    let runner = tempfile::tempdir().expect("runner workspace");
    for workspace in [controller.path(), runner.path()] {
        fs::write(workspace.join("tool"), "#!/bin/sh\n").expect("tool");
    }

    // A non-owner execute bit can be removed when tar extraction applies the
    // Linux runner's umask. It must not alter the cross-platform identity.
    fs::set_permissions(
        controller.path().join("tool"),
        fs::Permissions::from_mode(0o641),
    )
    .expect("controller permissions");
    fs::set_permissions(
        runner.path().join("tool"),
        fs::Permissions::from_mode(0o640),
    )
    .expect("runner permissions");
    assert_eq!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("controller hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("runner hash"),
        "non-owner execute bits are host metadata, not portable executable capability"
    );

    fs::set_permissions(
        runner.path().join("tool"),
        fs::Permissions::from_mode(0o740),
    )
    .expect("runner owner-executable permissions");
    assert_ne!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("controller hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("runner hash"),
        "owner execute changes remain fail-closed"
    );
}

#[test]
#[cfg(unix)]
fn snapshot_materialization_preserves_v3_owner_executable_capability_across_runner_umask() {
    use std::os::unix::fs::PermissionsExt;

    let controller = tempfile::tempdir().expect("macOS controller workspace");
    let runner = tempfile::tempdir().expect("Linux runner workspace");
    let controller_tool = controller.path().join("tool");
    let runner_tool = runner.path().join("tool");
    fs::write(&controller_tool, "#!/bin/sh\nexit 0\n").expect("controller tool");
    fs::set_permissions(&controller_tool, fs::Permissions::from_mode(0o755))
        .expect("controller executable permissions");

    let install = snapshot_install_command(&runner.path().display().to_string())
        .replacen("tar -p -C", "(umask 0111; tar -p -C", 1)
        .replacen("-xf - &&", "-xf -) &&", 1);
    let target = format!("sh -c {}", homeboy_core::engine::shell::quote_arg(&install));
    super::super::util::run_shell_command(
        &snapshot_archive_command(controller.path(), &target, &[]),
        "materialize restrictive-umask snapshot",
    )
    .expect("snapshot materialization");

    assert_eq!(
        fs::metadata(&runner_tool)
            .expect("runner tool metadata")
            .permissions()
            .mode()
            & 0o100,
        0o100,
        "the runner umask must not erase the v3-bound owner execute capability"
    );
    assert_eq!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("controller hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("runner hash"),
        "controller and runner must verify the same v3 canonical snapshot"
    );

    fs::set_permissions(&runner_tool, fs::Permissions::from_mode(0o644))
        .expect("remove runner executable capability");
    assert_ne!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("controller hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("runner executable-drift hash"),
        "meaningful owner-executable drift must remain fail-closed"
    );
}

#[test]
#[cfg(unix)]
fn workspace_content_hash_rejects_dereferenced_symlink_target_drift() {
    let controller = tempfile::tempdir().expect("controller workspace");
    let dependency = tempfile::tempdir().expect("dependency workspace");
    let target = dependency.path().join("tool");
    fs::write(&target, "first target\n").expect("first target contents");
    std::os::unix::fs::symlink(&target, controller.path().join("tool"))
        .expect("controller symlink");

    let expected = workspace_content_hash(controller.path(), &[]).expect("initial hash");
    fs::write(&target, "changed target\n").expect("changed target contents");
    assert_ne!(
        workspace_content_hash(controller.path(), &[]).expect("changed hash"),
        expected,
        "the dereferenced symlink target content remains provenance-bound"
    );
}

#[test]
#[cfg(unix)]
fn workspace_content_hash_versions_legacy_any_execute_separately_from_owner_execute() {
    use std::os::unix::fs::PermissionsExt;

    let controller = tempfile::tempdir().expect("controller workspace");
    let runner = tempfile::tempdir().expect("runner workspace");
    for workspace in [controller.path(), runner.path()] {
        fs::write(workspace.join("tool"), "#!/bin/sh\n").expect("tool");
    }
    fs::set_permissions(
        controller.path().join("tool"),
        fs::Permissions::from_mode(0o641),
    )
    .expect("controller permissions");
    fs::set_permissions(
        runner.path().join("tool"),
        fs::Permissions::from_mode(0o640),
    )
    .expect("runner permissions");

    assert_ne!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE
        )
        .expect("legacy controller hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE
        )
        .expect("legacy runner hash"),
        "v2 unix-executable preserves its historical any-execute semantics"
    );
    assert_eq!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE
        )
        .expect("v3 controller hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE
        )
        .expect("v3 runner hash"),
        "v3 owner-only executable capability normalizes non-owner execute bits"
    );
    assert_eq!(
        workspace_content_hash_algorithm(WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE).as_deref(),
        Some("homeboy-workspace-content-v2+unix-executable")
    );
    assert_eq!(
        workspace_content_hash_algorithm(WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE)
            .as_deref(),
        Some("homeboy-workspace-content-v3+unix-owner-executable")
    );
}

#[test]
fn workspace_content_manifest_contains_every_materialized_path() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(workspace.path().join("a".repeat(193)), "contents\n").expect("long path file");

    let manifest = workspace_content_manifest_for_policy(
        workspace.path(),
        &[],
        crate::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
    )
    .expect("content manifest");
    assert_eq!(manifest.entry_count, 1);
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(manifest.entries[0].bytes, Some(9));
    assert!(manifest.entries[0].sha256.is_some());
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
fn snapshot_content_hash_binds_user_owned_homeboy_files_but_ignores_runner_state() {
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

    fs::create_dir_all(destination.join(".homeboy/lab-at-files")).expect("Lab input directory");
    fs::write(
        destination.join(".homeboy/lab-at-files/plan.json"),
        "{\"task\":\"fixture\"}\n",
    )
    .expect("materialized Lab input");
    assert_eq!(
        workspace_content_hash(&destination, &excludes).expect("Lab input-insensitive hash"),
        expected,
        "broker-owned Lab @files must not change the controller identity"
    );
    let manifest = workspace_content_manifest_for_policy(
        &destination,
        &excludes,
        crate::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
    )
    .expect("materialized manifest");
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| !entry.path.starts_with(".homeboy/lab-at-files")),
        "broker-owned Lab @files must not enter the source manifest"
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
    let excluded_dependency_file = dependency.join("generated-state/secret.txt");
    std::fs::create_dir_all(dependency_file.parent().unwrap()).expect("dependency dir");
    std::fs::create_dir_all(excluded_dependency_file.parent().unwrap())
        .expect("excluded dependency dir");
    std::fs::write(&dependency_file, "#!/usr/bin/env node\n").expect("dependency file");
    std::fs::write(&excluded_dependency_file, "controller-only\n")
        .expect("excluded dependency file");
    std::fs::create_dir_all(source.join(".ci")).expect("ci dir");
    std::os::unix::fs::symlink(&dependency, source.join(".ci/dep")).expect("dep symlink");

    let destination = controller.path().join("snapshot");
    crate::workspace::snapshot::copy_snapshot_to_directory(
        &source,
        &destination,
        &[
            "generated-state".to_string(),
            "generated-state/**".to_string(),
        ],
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
    assert!(
        !destination
            .join(".ci/dep/generated-state/secret.txt")
            .exists(),
        "snapshot exclusions must also apply inside dereferenced dependencies"
    );
}
