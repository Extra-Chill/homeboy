use super::git;

use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::workspace::snapshot::{
    copy_snapshot_to_directory, ensure_no_runner_workspace_metadata_collision,
    immutable_replay_snapshot, materialize_snapshot_piped, materialize_snapshot_stage,
    register_after_snapshot_directory_discovery_hook, snapshot_input_manifest,
    snapshot_install_command, snapshot_overlay_install_command, snapshot_stable_manifest,
    synthetic_checkout_value, validate_snapshot_stability, workspace_content_hash,
    workspace_content_hash_algorithm, workspace_content_hash_for_policy, workspace_content_hash_v1,
    workspace_content_manifest_and_hash_for_policy, workspace_content_manifest_for_policy,
    WORKSPACE_CONTENT_PERMISSION_PORTABLE, WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
};

/// Parse `command` without executing it, under every POSIX shell on the host.
///
/// `sh -n` alone is a weak portability check. Where `/bin/sh` is bash, its
/// parser accepts bash-only syntax that dash rejects outright -- process
/// substitution `<(...)`, the `function` keyword, and array assignments -- so a
/// developer machine reports green while the runner's dash refuses the same
/// program. Preferring `dash` when it is installed makes the check as strict as
/// the shell that actually executes these commands (#10399).
///
/// A parse check cannot catch every bashism: `[[ ... ]]` and `((...))` parse as
/// ordinary words under dash and only fail when run. It does reliably catch the
/// structural errors this boundary keeps producing, including the assignment
/// prefix on a `(...)` subshell that both shells reject.
fn assert_parses_under_posix_shells(command: &str, label: &str) {
    let mut parsed_by = Vec::new();
    for shell in ["dash", "sh"] {
        let output = match std::process::Command::new(shell)
            .arg("-n")
            .arg("-c")
            .arg(command)
            .output()
        {
            Ok(output) => output,
            // `dash` is not installed everywhere; `sh` always is.
            Err(_) if shell == "dash" => continue,
            Err(error) => panic!("failed to run `{shell} -n` for {label}: {error}"),
        };
        assert!(
            output.status.success(),
            "{label} must parse under `{shell}`: {}\ncommand: {command}",
            String::from_utf8_lossy(&output.stderr)
        );
        parsed_by.push(shell);
    }
    assert!(
        parsed_by.contains(&"sh"),
        "{label} was never parsed by a shell"
    );
}

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

static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static SOURCE_SYNC_EXCLUDES_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
            Some(RunnerWorkspaceSyncMode::SnapshotGit.as_str())
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
fn snapshot_git_materializes_linked_worktree_with_valid_git_before_handoff() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source repository");
        let worktrees = tempfile::tempdir().expect("linked worktree root");
        let runner_root = tempfile::tempdir().expect("runner root");
        fs::write(source.path().join("file.txt"), "linked source\n").expect("source file");
        git(source.path(), &["init", "--quiet", "-b", "main"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        git(source.path(), &["add", "file.txt"]);
        git(source.path(), &["commit", "--quiet", "-m", "source"]);
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "file:///does-not-exist/linked-worktree.git",
            ],
        );
        let linked = worktrees.path().join("task-worktree");
        git(
            source.path(),
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().expect("linked path"),
                "HEAD",
            ],
        );
        assert!(
            linked.join(".git").is_file(),
            "provider-managed linked task worktree uses a gitdir pointer file"
        );
        let source_revision = git_output(&linked, &["rev-parse", "HEAD"]).expect("linked HEAD");
        crate::create(
            &format!(
                r#"{{"id":"lab-linked-snapshot-git","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (synced, exit_code) = sync_workspace(
            "lab-linked-snapshot-git",
            RunnerWorkspaceSyncOptions {
                path: linked.display().to_string(),
                mode: RunnerWorkspaceSyncMode::SnapshotGit,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("snapshot-git materializes a linked worktree");

        let remote = Path::new(&synced.remote_path);
        assert_eq!(exit_code, 0);
        assert_eq!(synced.sync_mode, RunnerWorkspaceSyncMode::SnapshotGit);
        assert!(
            remote.join(".git").is_dir(),
            "handoff workspace must own a Git directory rather than a copied gitdir pointer"
        );
        assert_eq!(
            git_output(remote, &["rev-parse", "--is-inside-work-tree"]).expect("work tree"),
            "true"
        );
        assert_eq!(
            git_output(remote, &["rev-parse", "--verify", "-q", "HEAD"]).expect("HEAD"),
            source_revision
        );
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).expect("linked source content"),
            "linked source\n"
        );
        super::super::util::verify_valid_git_representation(remote)
            .expect("verified valid .git representation before handoff");
    });
}

#[test]
fn snapshot_git_fails_before_handoff_when_controller_closure_is_unavailable() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source workspace");
        let runner_root = tempfile::tempdir().expect("runner root");
        fs::create_dir_all(source.path().join("components/demo")).expect("component root");
        fs::write(
            source.path().join("components/demo/component.txt"),
            "component\n",
        )
        .expect("component file");
        fs::write(source.path().join("dirty.txt"), "committed\n").expect("source file");
        git(source.path(), &["init", "-b", "main"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "source"]);
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "file:///definitely-not-a-runner-accessible-repository",
            ],
        );
        fs::write(source.path().join("dirty.txt"), "dirty\n").expect("dirty change");

        crate::create(
            &format!(
                r#"{{"id":"lab-snapshot-git-fallback","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let shim_root = tempfile::tempdir().expect("git shim root");
        let shim = shim_root.path().join("git");
        fs::write(
            &shim,
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = \"rev-list\" ] && { echo 'missing promisor object' >&2; exit 91; }; done\nexec /usr/bin/git \"$@\"\n",
        )
        .expect("write git shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&shim, fs::Permissions::from_mode(0o755))
                .expect("make git shim executable");
        }
        let original_path = std::env::var_os("PATH").expect("PATH");
        let mut paths = vec![shim_root.path().to_path_buf()];
        paths.extend(std::env::split_paths(&original_path));
        std::env::set_var("PATH", std::env::join_paths(paths).expect("PATH value"));
        let result = sync_workspace(
            "lab-snapshot-git-fallback",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::SnapshotGit,
                ..Default::default()
            },
        );
        std::env::set_var("PATH", original_path);

        let error = result.expect_err("closure failure must stop Lab handoff");
        assert!(
            error.message.contains("list git bundle objects"),
            "{error:?}"
        );
        let workspaces_root = runner_root.path().join("_lab_workspaces");
        assert!(
            !workspaces_root.exists()
                || fs::read_dir(workspaces_root)
                    .expect("read workspaces root")
                    .next()
                    .is_none(),
            "failed controller hydration must not hand off a fallback workspace"
        );
    });
}

/// A failing producer must fail the materialization even when the consumer
/// succeeds.
///
/// The archive pipeline is `(cd src && prepare && tar -cf - .) | ssh runner
/// 'tar -xf -'`. Under a plain POSIX shell the status is the *consumer's*, and
/// `tar -xf -` exits 0 on a truncated or empty stream — so a local `tar` that
/// died on a full disk or an unreadable file shipped a partial workspace and
/// reported success (#11100).
#[test]
fn a_failing_pipeline_producer_fails_the_command() {
    let error = super::super::util::run_shell_command(
        "sh -c 'printf partial; exit 19' | cat",
        "materialize SSH workspace snapshot",
    )
    .expect_err("a failing pipeline producer must fail the command");

    assert!(
        error.message.contains("exit status 19"),
        "the producer's status must be the reported status, not the consumer's: {}",
        error.message
    );
}

/// The consumer's failure must still win when it is the one that fails, so
/// `pipefail` does not mask the SSH-side transport errors that
/// `classify_transport_failure` depends on (#8803).
#[test]
fn a_failing_pipeline_consumer_still_fails_the_command() {
    let error = super::super::util::run_shell_command(
        "printf whole | sh -c 'exit 26'",
        "materialize SSH workspace snapshot",
    )
    .expect_err("a failing pipeline consumer must fail the command");

    assert!(
        error.message.contains("exit status 26"),
        "the consumer's status must survive: {}",
        error.message
    );
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
fn workspace_content_hash_skips_runner_metadata_removed_after_discovery() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(workspace.path().join("source.txt"), "source\n").expect("source file");
    let metadata = workspace.path().join(".homeboy");
    fs::create_dir_all(&metadata).expect("runner metadata directory");
    fs::write(metadata.join("runner-workspace.json"), "{}\n").expect("runner metadata");
    let expected = workspace_content_hash(workspace.path(), &[]).expect("baseline hash");

    let metadata_to_remove = metadata.clone();
    {
        let _hook = register_after_snapshot_directory_discovery_hook(
            metadata.canonicalize().expect("canonical metadata path"),
            move || {
                fs::remove_dir_all(&metadata_to_remove)
                    .expect("remove runner metadata during traversal");
            },
        );
        assert_eq!(
            workspace_content_hash(workspace.path(), &[])
                .expect("metadata cache miss is reconciled"),
            expected,
            "runner-owned metadata must not make an active snapshot claimant fail"
        );
    }

    fs::create_dir_all(&metadata).expect("restore runner metadata directory");
    fs::write(metadata.join("runner-workspace.json"), "{}\n").expect("restore runner metadata");
    let expected_v1 =
        workspace_content_hash_v1(workspace.path(), &[]).expect("legacy baseline hash");
    let metadata_to_remove = metadata.clone();
    {
        let _hook = register_after_snapshot_directory_discovery_hook(
            metadata.canonicalize().expect("canonical metadata path"),
            move || {
                fs::remove_dir_all(&metadata_to_remove)
                    .expect("remove runner metadata during legacy traversal");
            },
        );
        assert_eq!(
            workspace_content_hash_v1(workspace.path(), &[])
                .expect("legacy metadata cache miss is reconciled"),
            expected_v1,
            "legacy verification must tolerate the same runner metadata race"
        );
    }
}

#[test]
fn workspace_content_manifest_and_hash_share_one_traversal_instant() {
    let workspace = tempfile::tempdir().expect("workspace");
    let first = workspace.path().join("a-first.txt");
    let trigger = workspace.path().join("z-trigger.txt");
    fs::write(&first, "before\n").expect("first file");
    fs::write(&trigger, "trigger\n").expect("trigger file");

    let first_to_mutate = first.clone();
    let _hook = register_after_snapshot_directory_discovery_hook(trigger, move || {
        fs::write(first_to_mutate, "after\n").expect("mutate first file after its traversal");
    });
    let (manifest, identity) = workspace_content_manifest_and_hash_for_policy(
        workspace.path(),
        &[],
        WORKSPACE_CONTENT_PERMISSION_PORTABLE,
    )
    .expect("collect manifest and identity");

    let first_entry = manifest
        .entries
        .iter()
        .find(|entry| entry.path == "a-first.txt")
        .expect("first file manifest entry");
    assert_eq!(first_entry.bytes, Some(7));
    assert_ne!(
        identity,
        workspace_content_hash_for_policy(
            workspace.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_PORTABLE,
        )
        .expect("identity after mutation"),
        "the source mutation happened after the shared traversal read the first file"
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
fn snapshot_sync_excludes_late_injected_dmc_context_from_every_manifest() {
    let _source_sync_excludes_guard = SOURCE_SYNC_EXCLUDES_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("source sync excludes lock");
    let previous = std::env::var_os("HOMEBOY_SOURCE_SYNC_EXCLUDES");
    std::env::set_var(
        "HOMEBOY_SOURCE_SYNC_EXCLUDES",
        "./docs/superpowers/plans/**",
    );
    homeboy_core::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("clean DMC-managed worktree");
        let runner_root = tempfile::tempdir().expect("runner root");
        fs::create_dir_all(source.path().join("docs")).expect("docs directory");
        fs::write(source.path().join("docs/README.md"), "tracked docs\n").expect("tracked docs");
        fs::write(source.path().join("Cargo.toml"), "[workspace]\n").expect("worktree marker");
        git(source.path(), &["init", "-b", "main"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "clean source"]);

        crate::create(
            &format!(
                r#"{{"id":"lab-late-dmc-context","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let injected_context = source
            .path()
            .join("docs/superpowers/plans/2026-06-25-ftp2-page-reconstruct-lift-map.md");
        let injected_context_for_hook = injected_context.clone();
        let _hook = register_after_snapshot_directory_discovery_hook(
            source.path().join("docs"),
            move || {
                fs::create_dir_all(
                    injected_context_for_hook
                        .parent()
                        .expect("context parent directory"),
                )
                .expect("inject DMC context directory");
                fs::write(&injected_context_for_hook, "late DMC context\n")
                    .expect("inject DMC context file");
            },
        );

        let (output, exit_code) = sync_workspace(
            "lab-late-dmc-context",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                ..Default::default()
            },
        )
        .expect("late configured context must remain outside the snapshot");

        assert_eq!(exit_code, 0);
        assert!(output
            .excludes
            .contains(&"./docs/superpowers/plans/**".to_string()));
        assert!(
            injected_context.is_file(),
            "fixture injected context after discovery"
        );
        assert!(Path::new(&output.remote_path)
            .join("docs/README.md")
            .is_file());
        assert!(
            !Path::new(&output.remote_path)
                .join("docs/superpowers/plans/2026-06-25-ftp2-page-reconstruct-lift-map.md")
                .exists(),
            "the injected context must be absent from the staged and materialized manifests"
        );
    });
    match previous {
        Some(value) => std::env::set_var("HOMEBOY_SOURCE_SYNC_EXCLUDES", value),
        None => std::env::remove_var("HOMEBOY_SOURCE_SYNC_EXCLUDES"),
    }
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
            .contains("homeboy runner exec --cwd"));
        assert!(list.workspaces[0].exec_command.contains("-- <command>"));
    });
}

/// Regression for #8886: a cancelled/timed-out git materialization can leave a
/// checkout with no valid `HEAD`. `workspace list` must not advertise such a
/// partial checkout as reusable, because exec-ing against it fails with
/// "ambiguous argument 'HEAD'". Valid git checkouts and non-git snapshot
/// directories remain listed.
#[test]
fn workspace_list_omits_partial_git_checkouts_without_a_valid_head() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        crate::create(
            &format!(
                r#"{{"id":"lab-partial-list","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let lab_root = runner_root.path().join("_lab_workspaces");
        fs::create_dir_all(&lab_root).expect("lab workspaces root");

        // (a) A valid git checkout with a resolvable HEAD — must be listed.
        let valid = lab_root.join("valid-checkout");
        fs::create_dir_all(&valid).expect("valid dir");
        git(&valid, &["init", "-q"]);
        git(&valid, &["config", "user.email", "t@t"]);
        git(&valid, &["config", "user.name", "t"]);
        fs::write(valid.join("file.txt"), "hi\n").expect("file");
        git(&valid, &["add", "-A"]);
        git(&valid, &["commit", "-qm", "init"]);
        assert!(
            git_output(&valid, &["rev-parse", "--verify", "HEAD"]).is_ok(),
            "valid checkout must have a resolvable HEAD",
        );

        // (b) A partial checkout: a `.git` with no commit, so HEAD does not
        // resolve (exactly what a cancelled clone leaves) — must be omitted.
        let partial = lab_root.join("partial-checkout");
        fs::create_dir_all(&partial).expect("partial dir");
        git(&partial, &["init", "-q"]);
        assert!(
            git_output(&partial, &["rev-parse", "--verify", "HEAD"]).is_err(),
            "partial checkout must have no resolvable HEAD",
        );

        // (c) A non-git snapshot directory — must be listed.
        let snapshot_dir = lab_root.join("snapshot-workspace");
        fs::create_dir_all(&snapshot_dir).expect("snapshot dir");
        fs::write(snapshot_dir.join("Cargo.toml"), "[package]\n").expect("snapshot file");

        let (list, exit_code) = list_workspaces("lab-partial-list", 10).expect("list workspaces");
        assert_eq!(exit_code, 0);

        let listed: Vec<&str> = list
            .workspaces
            .iter()
            .map(|workspace| workspace.remote_path.as_str())
            .collect();

        assert!(
            listed.iter().any(|path| path.ends_with("valid-checkout")),
            "valid git checkout must be listed as reusable: {listed:?}",
        );
        assert!(
            listed
                .iter()
                .any(|path| path.ends_with("snapshot-workspace")),
            "non-git snapshot workspace must be listed as reusable: {listed:?}",
        );
        assert!(
            !listed.iter().any(|path| path.ends_with("partial-checkout")),
            "partial git checkout without a valid HEAD must NOT be advertised as reusable: {listed:?}",
        );
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
fn snapshot_git_hydrates_partial_clone_on_controller_without_lab_remote_access() {
    let _path_guard = PATH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PATH lock");
    homeboy_core::test_support::with_isolated_home(|_| {
        let origin = tempfile::tempdir().expect("origin tempdir");
        let author = tempfile::tempdir().expect("author tempdir");
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        // Initialize the bare origin on `main` so its HEAD symref matches the
        // branch the author pushes. Without an explicit `-b main`, git falls back
        // to the host `init.defaultBranch` (often `master`), leaving the partial
        // clone below with an unborn default branch and no HEAD to inspect.
        git(origin.path(), &["init", "--bare", "-b", "main"]);
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
        let lab_remote_access = shim_root.path().join("lab-remote-accessed");
        let origin_url = format!("file://{}", origin.path().display());
        fs::write(
            &shim,
            format!(
                "#!/bin/sh\nif [ \"$1\" = clone ]; then for arg in \"$@\"; do [ \"$arg\" = {origin_url:?} ] && {{ : > {}; exit 91; }}; done; fi\nexec /usr/bin/git \"$@\"\n",
                lab_remote_access.display(),
            ),
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
                mode: RunnerWorkspaceSyncMode::SnapshotGit,
                ..Default::default()
            },
        )
        .expect("controller must hydrate and bundle the partial clone");
        std::env::set_var("PATH", original_path);

        let remote = Path::new(&output.remote_path);
        assert_eq!(exit_code, 0);
        assert!(
            output.materialization_plan.controller_git_bundle.is_some(),
            "snapshot-git handoff must retain controller bundle provenance"
        );
        assert_eq!(
            output
                .materialization_plan
                .actual_materialization_mode
                .as_deref(),
            Some("snapshot-git")
        );
        assert!(remote.join(".git").exists());
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
        let metadata: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(remote.join(".homeboy/runner-workspace.json"))
                .expect("read workspace metadata"),
        )
        .expect("parse workspace metadata");
        assert_eq!(metadata["actual_materialization_mode"], "snapshot-git");
        assert_eq!(
            metadata["source_remote_url"],
            format!("file://{}", origin.path().display())
        );
        assert!(
            !git_output(
                source.path(),
                &["rev-list", "--objects", "--missing=print", "HEAD"]
            )
            .expect("inspect hydrated controller")
            .contains(&format!("?{base_blob}")),
            "controller must hydrate every promised snapshot object before handoff"
        );
        assert!(
            !lab_remote_access.exists(),
            "Lab materializer must not contact the promisor remote"
        );
        assert!(
            git_output(
                remote,
                &["config", "--get-regexp", r"^remote\..*\.promisor$"]
            )
            .is_err(),
            "Lab checkout must not inherit promisor remote configuration"
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

/// The install commands cross the same `sh -c` boundary as the archive
/// pipeline, on both the local and the SSH runner path, so hold them to the
/// same portability contract (#10399).
#[test]
fn snapshot_install_commands_parse_under_posix_shells() {
    for remote_path in [
        "/var/lib/sampleplugin/workspace/_lab_workspaces/homeboy-abc",
        "/var/lib/homeboy/work spaces/homeboy-abc",
    ] {
        assert_parses_under_posix_shells(
            &snapshot_install_command(remote_path),
            "snapshot install command",
        );
        assert_parses_under_posix_shells(
            &snapshot_overlay_install_command(remote_path),
            "snapshot overlay install command",
        );
    }
}

#[test]
fn snapshot_staging_rejects_a_disappearing_runtime_overlay_before_ssh() {
    let source = tempfile::tempdir().expect("snapshot source");
    let overlay = source.path().join("runtime-overlays");
    fs::create_dir_all(&overlay).expect("runtime overlay source");
    fs::write(overlay.join("runtime.js"), "runtime").expect("runtime artifact");

    // This is the race that previously became `tar: ./runtime-overlays: Cannot
    // stat` in a pipeline whose SSH side had already started. The typed manifest
    // preserves the declaration identity and staging now fails before transport.
    let manifest = snapshot_input_manifest(source.path(), &[]).expect("input manifest");
    fs::remove_dir_all(&overlay).expect("remove overlay after manifest creation");
    let error = materialize_snapshot_stage(source.path(), &[], &manifest, None)
        .expect_err("missing declared overlay must fail during local staging");

    assert_eq!(error.retryable, Some(false));
    assert_eq!(error.details["classification"], "snapshot_construction");
    assert_eq!(error.details["declaration_id"], "root:runtime-overlays");
    assert_eq!(
        error.details["source_identity"],
        serde_json::json!(overlay.display().to_string())
    );
    assert!(error.details["staging_output"]
        .as_str()
        .is_some_and(|path| path.ends_with("/source")));
    assert!(error.details["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("tar: ./runtime-overlays: Cannot stat")));
    assert_eq!(
        error.details["recovery"]["action"],
        "rebuild_snapshot_staging_and_replay_cook"
    );
}

#[test]
fn snapshot_transport_archives_only_the_admitted_scratch_stage() {
    let source = tempfile::tempdir().expect("source");
    let scratch = tempfile::tempdir().expect("admitted scratch");
    let archive_list = tempfile::NamedTempFile::new().expect("archive list");
    fs::write(source.path().join("runtime-overlays"), "runtime").expect("overlay");
    fs::write(source.path().join("other"), "other").expect("other input");
    let manifest = snapshot_input_manifest(source.path(), &[]).expect("input manifest");
    let stage = materialize_snapshot_stage(source.path(), &[], &manifest, Some(scratch.path()))
        .expect("stage in admitted scratch");
    assert!(stage.path().starts_with(scratch.path()));

    materialize_snapshot_piped(
        source.path(),
        &format!(
            "tar -tf - > {}",
            homeboy_core::engine::shell::quote_arg(&archive_list.path().display().to_string())
        ),
        &[],
        "test SSH snapshot transport",
        Some(scratch.path()),
    )
    .expect("stage then transport snapshot");

    let entries = fs::read_to_string(archive_list.path()).expect("tar input list");
    assert!(entries.contains("runtime-overlays"));
    assert!(entries.contains("other"));
}

#[cfg(unix)]
#[test]
fn immutable_replay_snapshot_rejects_external_symlinks_before_staging() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    let external = tempfile::NamedTempFile::new().expect("external");
    symlink(external.path(), source.path().join("external-link")).expect("link external file");

    let error = immutable_replay_snapshot(source.path(), &[])
        .expect_err("external symlink must not enter replay artifact");
    assert!(error.message.contains("refused a symlink"));
}

#[cfg(unix)]
#[test]
fn immutable_replay_snapshot_rejects_untracked_internal_symlinks_before_staging() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    fs::write(source.path().join("tracked.txt"), "workspace bytes").expect("workspace file");
    symlink("tracked.txt", source.path().join("untracked-link")).expect("link workspace file");

    let error = immutable_replay_snapshot(source.path(), &[])
        .expect_err("untracked symlink must not enter replay artifact");
    assert!(error.message.contains("refused a symlink"));
}

#[test]
fn immutable_replay_snapshot_is_unchanged_when_source_is_mutated_and_restored() {
    let source = tempfile::tempdir().expect("source");
    let input = source.path().join("input.txt");
    fs::write(&input, "recorded").expect("write input");
    let snapshot = immutable_replay_snapshot(source.path(), &[]).expect("seal replay artifact");

    fs::write(&input, "transfer-time mutation").expect("mutate source");
    fs::write(&input, "recorded").expect("restore source");

    assert_eq!(
        fs::read_to_string(snapshot.path().join("input.txt")).expect("read sealed artifact"),
        "recorded"
    );
}

#[test]
fn immutable_replay_snapshot_identity_and_bytes_honor_exclusions() {
    let source = tempfile::tempdir().expect("source");
    fs::write(source.path().join("included.txt"), "included").expect("included input");
    fs::write(source.path().join("excluded.txt"), "first").expect("excluded input");
    let excludes = vec!["excluded.txt".to_string()];
    let first = immutable_replay_snapshot(source.path(), &excludes).expect("first artifact");
    fs::write(source.path().join("excluded.txt"), "second").expect("change excluded input");
    let second = immutable_replay_snapshot(source.path(), &excludes).expect("second artifact");

    assert_eq!(first.identity, second.identity);
    assert!(!second.path().join("excluded.txt").exists());
}

#[cfg(unix)]
#[test]
fn immutable_replay_snapshot_rejects_regular_file_swapped_to_symlink_during_copy() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    let _lock = replay_snapshot_hook_lock();
    let input = source.path().join("input.txt");
    let external = tempfile::NamedTempFile::new().expect("external");
    fs::write(&input, "recorded").expect("write regular input");
    let _hook = register_after_snapshot_directory_discovery_hook(input.clone(), move || {
        fs::remove_file(&input).expect("remove regular input");
        symlink(external.path(), &input).expect("replace input with symlink");
    });

    let error = immutable_replay_snapshot(source.path(), &[])
        .expect_err("regular-to-symlink swap must fail closed");
    assert!(error.message.contains("refused a symlink"));
}

#[cfg(unix)]
#[test]
fn immutable_replay_snapshot_rejects_directory_swapped_to_external_symlink_during_archive() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    let _lock = replay_snapshot_hook_lock();
    let directory = source.path().join("directory");
    let external = tempfile::tempdir().expect("external");
    fs::create_dir(&directory).expect("create source directory");
    fs::write(directory.join("input.txt"), "recorded").expect("write source file");
    let _hook = register_after_snapshot_directory_discovery_hook(directory.clone(), move || {
        fs::remove_dir_all(&directory).expect("remove source directory");
        symlink(external.path(), &directory).expect("replace directory with external symlink");
    });

    let error = immutable_replay_snapshot(source.path(), &[])
        .expect_err("directory-to-symlink swap must fail closed");
    assert!(error.message.contains("refused a symlink"));
}

#[cfg(unix)]
#[test]
fn immutable_replay_snapshot_preserves_executable_files() {
    use std::os::unix::fs::PermissionsExt;

    let source = tempfile::tempdir().expect("source");
    let script = source.path().join("script.sh");
    fs::write(&script, "#!/bin/sh\nexit 0\n").expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
        .expect("make script executable");

    let snapshot = immutable_replay_snapshot(source.path(), &[]).expect("seal replay artifact");
    assert_ne!(
        fs::metadata(snapshot.path().join("script.sh"))
            .expect("staged script metadata")
            .permissions()
            .mode()
            & 0o100,
        0
    );
}

#[test]
fn snapshot_staging_keeps_nested_root_ignored_outputs_out_of_repeated_snapshots() {
    let workspace = tempfile::tempdir().expect("workspace");
    let source = workspace.path().join("source");
    fs::create_dir_all(source.join("src/generated-output")).expect("generated output directory");
    fs::write(source.join("src/lib.rs"), "pub fn stable() {}\n").expect("source file");
    fs::write(source.join("src/generated-output/state.json"), "ignored\n").expect("ignored output");
    for index in 0..128 {
        let sibling = source.join(format!("worktrees/sibling-{index}"));
        fs::create_dir_all(&sibling).expect("sibling directory");
        fs::write(sibling.join("build-output"), "ignored\n").expect("sibling output");
    }

    let excludes = vec![
        "./src/generated-output".to_string(),
        "./worktrees".to_string(),
    ];
    let baseline = snapshot_stable_manifest(&source, &excludes).expect("baseline manifest");
    for destination in [
        workspace.path().join("first"),
        workspace.path().join("second"),
    ] {
        copy_snapshot_to_directory(&source, &destination, &excludes)
            .expect("ignored outputs must not enter the staging archive");
        assert_eq!(
            snapshot_stable_manifest(&destination, &[]).expect("staged manifest"),
            baseline,
            "repeated snapshot must preserve the source manifest"
        );
        assert!(!destination.join("src/generated-output").exists());
        assert!(!destination.join("worktrees").exists());
    }
}

#[test]
fn snapshot_staging_uses_one_manifest_policy_for_nested_ignored_directories() {
    let workspace = tempfile::tempdir().expect("workspace");
    let source = workspace.path().join("source");
    let ignored = source.join("docs/superpowers/plans/ignored.md");
    fs::create_dir_all(ignored.parent().expect("ignored parent")).expect("ignored directory");
    fs::write(source.join("README.md"), "tracked\n").expect("tracked source");
    fs::write(&ignored, "ignored\n").expect("ignored source");
    let excludes = vec!["**/docs/superpowers/".to_string()];

    let before = snapshot_stable_manifest(&source, &excludes).expect("source manifest");
    let manifest = snapshot_input_manifest(&source, &excludes).expect("input manifest");
    let stage = materialize_snapshot_stage(&source, &excludes, &manifest, None).expect("stage");
    let staged =
        snapshot_stable_manifest(&stage.path().join("source"), &excludes).expect("staged manifest");
    let after = snapshot_stable_manifest(&source, &excludes).expect("current manifest");

    validate_snapshot_stability(
        &before,
        &staged,
        &after,
        &source,
        &stage.path().join("source"),
    )
    .expect("ignored paths use one manifest policy");

    let provider_workspace = workspace.path().join("provider-workspace");
    fs::create_dir_all(&provider_workspace).expect("provider workspace");
    materialize_snapshot_piped(
        &source,
        &format!(
            "tar -C {} -xf -",
            homeboy_core::engine::shell::quote_arg(&provider_workspace.display().to_string())
        ),
        &excludes,
        "test snapshot transport",
        None,
    )
    .expect("snapshot reaches provider transport");
    assert!(provider_workspace.join("README.md").is_file());
    assert!(!provider_workspace
        .join("docs/superpowers/plans/ignored.md")
        .exists());
}

#[test]
fn lab_snapshot_preacceptance_preserves_tracked_build_sources_before_provider_execution() {
    let workspace = tempfile::tempdir().expect("workspace");
    let source = workspace.path().join("source");
    let tracked_build = source.join("crates/homeboy-extension/src/build");
    fs::create_dir_all(&tracked_build).expect("tracked build source directory");
    fs::create_dir_all(source.join("build")).expect("ignored root build directory");
    fs::write(tracked_build.join("mod.rs"), "pub mod local_permissions;\n")
        .expect("tracked build source");
    fs::write(source.join("build/generated.bin"), "ignored\n").expect("ignored build output");
    let excludes = vec!["./build".to_string(), "./build/**".to_string()];

    let source_manifest = snapshot_stable_manifest(&source, &excludes).expect("source manifest");
    let input_manifest = snapshot_input_manifest(&source, &excludes).expect("input manifest");
    let stage = materialize_snapshot_stage(&source, &excludes, &input_manifest, None)
        .expect("snapshot preacceptance stage");
    let staged_source = stage.path().join("source");
    let staged_manifest = snapshot_stable_manifest(&staged_source, &[]).expect("staged manifest");
    let current_manifest = snapshot_stable_manifest(&source, &excludes).expect("current manifest");
    assert_eq!(source_manifest, staged_manifest);
    assert_eq!(staged_manifest, current_manifest);
    assert!(staged_source
        .join("crates/homeboy-extension/src/build/mod.rs")
        .is_file());
    assert!(!staged_source.join("build").exists());

    let provider_workspace = workspace.path().join("provider-workspace");
    let provider_marker = workspace.path().join("provider-executed");
    fs::create_dir_all(&provider_workspace).expect("provider workspace");
    let target_command = format!(
        "tar -C {} -xf - && test -f {} && test ! -e {} && touch {}",
        homeboy_core::engine::shell::quote_arg(&provider_workspace.display().to_string()),
        homeboy_core::engine::shell::quote_arg(
            &provider_workspace
                .join("crates/homeboy-extension/src/build/mod.rs")
                .display()
                .to_string()
        ),
        homeboy_core::engine::shell::quote_arg(
            &provider_workspace.join("build").display().to_string()
        ),
        homeboy_core::engine::shell::quote_arg(&provider_marker.display().to_string()),
    );
    materialize_snapshot_piped(
        &source,
        &target_command,
        &excludes,
        "test Lab provider execution",
        None,
    )
    .expect("snapshot preacceptance reaches provider execution");
    assert!(provider_marker.is_file());
}

#[test]
fn snapshot_construction_failure_does_not_start_transport_or_accept_source_drift() {
    let source = tempfile::tempdir().expect("source");
    let scratch = tempfile::tempdir().expect("admitted scratch");
    let marker = tempfile::NamedTempFile::new().expect("marker");
    fs::remove_file(marker.path()).expect("remove marker");
    let missing_source = source.path().join("missing-workspace");
    let error = materialize_snapshot_piped(
        &missing_source,
        &format!("touch {}", marker.path().display()),
        &[],
        "test SSH snapshot transport",
        Some(scratch.path()),
    )
    .expect_err("construction failure must reject transport");
    assert_eq!(error.details["classification"], "snapshot_construction");
    assert!(
        !marker.path().exists(),
        "transport must not start after staging failure"
    );
}

#[test]
fn snapshot_stability_rejects_a_mixed_staged_tree_even_if_source_is_restored() {
    let source = tempfile::tempdir().expect("source");
    let scratch = tempfile::tempdir().expect("scratch");
    fs::write(source.path().join("runtime-overlays"), "before").expect("overlay");
    let before = snapshot_stable_manifest(source.path(), &[]).expect("before manifest");
    let manifest = snapshot_input_manifest(source.path(), &[]).expect("input manifest");
    let stage = materialize_snapshot_stage(source.path(), &[], &manifest, Some(scratch.path()))
        .expect("stage");
    fs::write(stage.path().join("source/runtime-overlays"), "mixed").expect("mutate stage");
    fs::write(source.path().join("runtime-overlays"), "after").expect("mutate source");
    fs::write(source.path().join("runtime-overlays"), "before").expect("restore source");
    let staged =
        snapshot_stable_manifest(&stage.path().join("source"), &[]).expect("staged manifest");
    let after = snapshot_stable_manifest(source.path(), &[]).expect("after manifest");
    let error = validate_snapshot_stability(
        &before,
        &staged,
        &after,
        source.path(),
        &stage.path().join("source"),
    )
    .expect_err("mixed staged tree must fail even after source ABA restoration");
    assert_eq!(error.details["classification"], "snapshot_construction");
    assert!(
        error.message.contains("bounded differing entries"),
        "{error:?}"
    );
    assert!(error.message.contains("runtime-overlays"), "{error:?}");
}

#[test]
fn snapshot_staging_preserves_an_admitted_root_when_every_child_is_excluded() {
    let source = tempfile::tempdir().expect("source");
    let overlays = source.path().join("runtime-overlays/php-wasm");
    fs::create_dir_all(&overlays).expect("runtime overlay directory");
    fs::write(overlays.join("runtime.wasm"), b"\0asm").expect("runtime artifact");
    let excludes = vec!["runtime-overlays/*".to_string()];

    let before = snapshot_stable_manifest(source.path(), &excludes).expect("source manifest");
    let manifest = snapshot_input_manifest(source.path(), &excludes).expect("input manifest");
    let stage = materialize_snapshot_stage(source.path(), &excludes, &manifest, None)
        .expect("snapshot stage");
    let staged_source = stage.path().join("source");
    let staged = snapshot_stable_manifest(&staged_source, &excludes).expect("staged manifest");
    let after = snapshot_stable_manifest(source.path(), &excludes).expect("current manifest");

    validate_snapshot_stability(&before, &staged, &after, source.path(), &staged_source)
        .expect("the excluded children retain their admitted empty root");
    assert!(staged_source.join("runtime-overlays").is_dir());
    assert!(!staged_source.join("runtime-overlays/php-wasm").exists());
}

#[test]
fn snapshot_staging_is_stable_with_sibling_worktrees_and_ignored_outputs() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let workspace_root = tempfile::tempdir().expect("workspace root");
        let source = workspace_root.path().join("homeboy@task");
        let runner_root = tempfile::tempdir().expect("runner root");
        fs::create_dir_all(&source).expect("source directory");
        fs::write(source.join("tracked.txt"), "source\n").expect("tracked source");
        fs::write(source.join(".gitignore"), "generated/\ntarget/\n").expect("gitignore");
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test User"]);
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "baseline"]);
        for index in 0..16 {
            let sibling = workspace_root
                .path()
                .join(format!("homeboy@sibling-{index}"));
            let output = std::process::Command::new("git")
                .args(["worktree", "add", "--detach"])
                .arg(&sibling)
                .arg("HEAD")
                .current_dir(&source)
                .output()
                .expect("create sibling worktree");
            assert!(
                output.status.success(),
                "create sibling worktree: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::create_dir_all(source.join("generated/runtime")).expect("generated directory");
        fs::create_dir_all(source.join("target/debug")).expect("target directory");
        fs::write(source.join("generated/runtime/output.js"), "generated\n")
            .expect("generated output");
        fs::write(source.join("target/debug/homeboy"), "binary\n").expect("target output");

        crate::create(
            &format!(
                r#"{{"id":"lab-stable-clean-worktree","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");
        let options = RunnerWorkspaceSyncOptions {
            path: source.display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        };

        for attempt in 0..3 {
            let (synced, exit_code) = sync_workspace("lab-stable-clean-worktree", options.clone())
                .expect("clean worktree snapshot must remain stable");
            assert_eq!(exit_code, 0, "attempt {attempt}");
            let staged = Path::new(&synced.remote_path);
            assert!(staged.join("tracked.txt").is_file());
            assert!(!staged.join("generated/runtime/output.js").exists());
            assert!(!staged.join("target/debug/homeboy").exists());
        }
    });
}

#[cfg(unix)]
#[test]
fn content_hash_binds_tracked_unresolved_symlinks_deterministically() {
    use std::os::unix::fs::symlink;

    // A tracked symlink whose target is intentionally unavailable on the
    // controller (e.g. `blogs.dir -> /nfs`) is a valid Git workspace shape and
    // must not fail content hashing. (#8374)
    let dir = tempfile::tempdir().expect("workspace");
    symlink("/nfs", dir.path().join("blogs.dir")).expect("unresolved symlink");
    fs::write(dir.path().join("regular.txt"), "content\n").expect("regular file");

    let hash = workspace_content_hash(dir.path(), &[])
        .expect("an unresolved tracked symlink must not fail content hashing");
    assert!(!hash.is_empty());
    // Deterministic: re-hashing the same shape yields the same identity.
    let repeat = workspace_content_hash(dir.path(), &[]).expect("repeat hash");
    assert_eq!(hash, repeat, "content hash must be deterministic");
    // The legacy v1 algorithm must also bind it rather than refuse.
    workspace_content_hash_v1(dir.path(), &[]).expect("v1 must also bind the symlink");

    // The symlink target text is part of the identity: changing it changes hash.
    let other = tempfile::tempdir().expect("other workspace");
    symlink("/different-target", other.path().join("blogs.dir")).expect("other symlink");
    fs::write(other.path().join("regular.txt"), "content\n").expect("regular file");
    let other_hash = workspace_content_hash(other.path(), &[]).expect("other hash");
    assert_ne!(
        hash, other_hash,
        "a different symlink target must change the content hash"
    );
}

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
fn snapshot_git_omits_absent_optional_root_context_paths() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let optional_paths = ["/.datamachine", "/.claude", "/AGENTS.md", "/.git"];

        for (index, ignored_paths) in optional_paths
            .iter()
            .map(|path| vec![*path])
            .chain(std::iter::once(optional_paths.to_vec()))
            .enumerate()
        {
            let source = tempfile::tempdir().expect("source workspace");
            let runner_root = tempfile::tempdir().expect("runner root");
            fs::write(source.path().join("README.md"), "source\n").expect("source file");
            fs::write(source.path().join(".gitignore"), ignored_paths.join("\n"))
                .expect("optional context excludes");
            git(source.path(), &["init", "-b", "main"]);
            git(source.path(), &["config", "user.email", "test@example.com"]);
            git(source.path(), &["config", "user.name", "Test User"]);
            git(source.path(), &["add", "."]);
            git(source.path(), &["commit", "-m", "source"]);

            crate::create(
                &format!(
                    r#"{{"id":"lab-optional-context-{index}","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                &format!("lab-optional-context-{index}"),
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::SnapshotGit,
                    ..Default::default()
                },
            )
            .expect("absent optional context paths must not block SnapshotGit materialization");

            assert_eq!(exit_code, 0);
            assert!(Path::new(&output.remote_path).join("README.md").exists());
        }
    });
}

#[cfg(unix)]
#[test]
fn snapshot_archive_fails_for_existing_unreadable_input() {
    use std::os::unix::fs::PermissionsExt;

    let source = tempfile::tempdir().expect("source workspace");
    let destination = tempfile::tempdir().expect("destination workspace");
    let unreadable = source.path().join("unreadable.txt");
    fs::write(&unreadable, "private\n").expect("unreadable source file");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000))
        .expect("remove source read permission");

    if fs::read(&unreadable).is_ok() {
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600))
            .expect("restore source permission");
        return;
    }

    let error = copy_snapshot_to_directory(source.path(), destination.path(), &[])
        .expect_err("an existing unreadable source input must fail materialization");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o600))
        .expect("restore source permission");

    assert!(error.message.contains("prepare local workspace snapshot"));
    assert!(error.message.contains("Permission denied"));
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
#[cfg(unix)]
fn snapshot_content_hash_is_portable_when_transport_drops_owner_execute() {
    use std::os::unix::fs::PermissionsExt;

    let controller = tempfile::tempdir().expect("controller workspace");
    let runner = tempfile::tempdir().expect("runner workspace");
    for workspace in [controller.path(), runner.path()] {
        fs::write(workspace.join("tool"), "#!/bin/sh\necho homeboy\n").expect("tool");
    }
    fs::set_permissions(
        controller.path().join("tool"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("controller permissions");
    fs::set_permissions(
        runner.path().join("tool"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("runner permissions");

    assert_ne!(
        workspace_content_hash_for_policy(
            controller.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("controller Unix hash"),
        workspace_content_hash_for_policy(
            runner.path(),
            &[],
            WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
        )
        .expect("runner Unix hash"),
        "Unix mode provenance is not portable through a cross-platform snapshot"
    );
    assert_eq!(
        workspace_content_hash(controller.path(), &[]).expect("controller portable hash"),
        workspace_content_hash(runner.path(), &[]).expect("runner portable hash"),
        "new snapshots bind the portable content contract"
    );

    fs::write(runner.path().join("tool"), "#!/bin/sh\necho changed\n").expect("mutate tool");
    assert_ne!(
        workspace_content_hash(controller.path(), &[]).expect("controller portable hash"),
        workspace_content_hash(runner.path(), &[]).expect("changed runner portable hash"),
        "portable identity remains fail-closed for content changes"
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
fn replay_snapshot_hook_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("replay snapshot hook lock")
}
