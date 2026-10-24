use homeboy_core::api_jobs::RemoteRunnerJobRequest;
use sha2::{Digest, Sha256};

use super::{
    bind_materialized_cook_workspace_identity, open_staged_workspace_directory,
    verify_private_at_files, verify_private_command_arguments, verify_staged_workspace_path,
    write_private_at_file_snapshot,
};

#[cfg(unix)]
#[test]
fn staged_cook_attestation_binds_materialized_git_identity_and_rejects_later_drift() {
    use homeboy_agents::agent_task_workspace_identity::attest_workspace;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    crate::test_support::with_isolated_home(|home| {
        let repository = home.path().join("repository");
        let source = home.path().join("controller-worktree");
        let runner = home.path().join("runner-workspace");
        let git = |cwd: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "-q"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "baseline",
            ],
        );
        git(
            &repository,
            &["worktree", "add", "--detach", source.to_str().unwrap()],
        );
        git(
            home.path(),
            &[
                "clone",
                "--local",
                repository.to_str().unwrap(),
                runner.to_str().unwrap(),
            ],
        );
        let source_identity = attest_workspace(&source).unwrap();
        assert_eq!(source_identity["git_representation"], "pointer_file");
        assert_eq!(
            attest_workspace(&runner).unwrap()["git_representation"],
            "directory"
        );
        let mut transported_identity = source_identity.clone();
        transported_identity["canonical_path"] = serde_json::json!(runner.canonicalize().unwrap());
        let plan = serde_json::json!({
            "schema": "homeboy/agent-task-plan/v1", "plan_id": "cross-host",
            "tasks": [{ "schema": "homeboy/agent-task-request/v1", "task_id": "task",
                "instructions": "private fixture", "executor": {"backend": "fixture"},
                "workspace": {"root": runner}, "metadata": {
                    "cook_workspace_identity": transported_identity,
                    "workspace_source_provenance": { "controller_root": source,
                        "cook_workspace_identity": source_identity }
                }
            }]
        });
        let bytes = serde_json::to_vec(&plan).unwrap();
        let file = home.path().join(format!(
            "private-sha256-{:x}-plan.json",
            Sha256::digest(&bytes)
        ));
        std::fs::write(&file, &bytes).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let request = RemoteRunnerJobRequest {
            runner_id: "lab".to_string(),
            project_id: None,
            operation: "runner.exec".to_string(),
            command: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                format!("@{}", file.display()),
            ],
            cwd: Some(runner.display().to_string()),
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: Default::default(),
            env_materialization: None,
            capture_patch: false,
            source_snapshot: None,
            path_materialization_plan: None,
            require_paths: Vec::new(),
            extension_env_providers: Vec::new(),
            lab_runner_workload: None,
            lifecycle: None,
            workspace_claim_binding: None,
            workspace_owner_lease: None,
            metadata: None,
        };
        let mut envelope = request.execution_envelope();
        assert!(crate::lab_args::verify_cook_workspace_attestations_in_args(
            &envelope.dispatch.as_ref().unwrap().command,
            home.path()
        )
        .is_err());
        let cleanup =
            bind_materialized_cook_workspace_identity(&mut envelope, runner.to_str().unwrap())
                .unwrap();
        let args = &envelope.dispatch.as_ref().unwrap().command;
        crate::lab_args::verify_cook_workspace_attestations_in_args(args, home.path()).unwrap();
        let snapshot = args[4].strip_prefix('@').unwrap();
        let bound: serde_json::Value =
            serde_json::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
        assert_eq!(
            bound["tasks"][0]["metadata"]["workspace_source_provenance"],
            plan["tasks"][0]["metadata"]["workspace_source_provenance"]
        );
        assert_eq!(
            std::fs::metadata(snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let authority = open_staged_workspace_directory(&runner).unwrap();
        std::fs::rename(&runner, home.path().join("old-workspace")).unwrap();
        git(
            home.path(),
            &[
                "clone",
                "--local",
                repository.to_str().unwrap(),
                runner.to_str().unwrap(),
            ],
        );
        assert!(verify_staged_workspace_path(&authority, runner.to_str().unwrap()).is_err());
        assert!(
            crate::lab_args::verify_cook_workspace_attestations_in_args(args, home.path()).is_err()
        );
        drop(cleanup);
        assert!(!std::path::Path::new(snapshot).exists());
    });
}

#[cfg(unix)]
#[test]
fn retained_staged_workspace_authority_refuses_path_replacement_before_spawn() {
    let root = tempfile::tempdir().expect("workspace root");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::write(workspace.join("source.bin"), b"sealed source").expect("write source");
    let authority = open_staged_workspace_directory(&workspace).expect("retain workspace");

    std::fs::rename(&workspace, root.path().join("replaced")).expect("move workspace");
    std::fs::create_dir(&workspace).expect("replace workspace path");
    std::fs::write(workspace.join("source.bin"), b"replacement").expect("write replacement");

    let error = verify_staged_workspace_path(&authority, &workspace.display().to_string())
        .expect_err("replacement must be refused before spawn");
    assert!(error.message.contains("was replaced after materialization"));
}

#[test]
fn private_at_file_verifier_preserves_commands_without_private_inputs() {
    let mut command = vec![
        "sh".to_string(),
        "-c".to_string(),
        "printf unchanged".to_string(),
        "@missing-public-input.json".to_string(),
    ];
    let original = command.clone();
    let _cleanup = verify_private_command_arguments(&mut command)
        .expect("non-private command needs no filesystem inputs");
    assert_eq!(command, original);
}

#[cfg(unix)]
#[test]
fn private_at_file_snapshot_survives_source_replacement_before_exec() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");
    let verified = b"verified plan";
    let digest = format!("{:x}", Sha256::digest(verified));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    std::fs::write(&source, verified).expect("write private plan");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600))
        .expect("lock private plan");
    let request = RemoteRunnerJobRequest {
        runner_id: "lab".to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: vec!["sh".to_string(), format!("@{}", source.display())],
        cwd: Some("/tmp".to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
        path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        workspace_claim_binding: None,
        workspace_owner_lease: None,
        metadata: None,
    };
    let mut envelope = request.execution_envelope();

    let cleanup = verify_private_at_files(&mut envelope).expect("verify private input");
    let snapshot = envelope.dispatch.as_ref().expect("dispatch").command[1]
        .strip_prefix('@')
        .expect("rewritten @file")
        .to_string();
    assert_ne!(
        std::path::Path::new(&snapshot).parent(),
        source.parent(),
        "verified snapshot is outside the source-owner directory"
    );
    assert_eq!(
        std::fs::metadata(
            std::path::Path::new(&snapshot)
                .parent()
                .expect("snapshot parent")
        )
        .expect("snapshot parent metadata")
        .permissions()
        .mode()
            & 0o777,
        0o700,
        "worker-owned snapshot directory is private"
    );
    std::fs::write(&source, b"replaced after validation").expect("replace source");

    assert_eq!(std::fs::read(&snapshot).expect("read snapshot"), verified);
    drop(cleanup);
    assert!(!source.exists(), "source cleanup runs after replacement");
    assert!(
        !std::path::Path::new(&snapshot).exists(),
        "snapshot cleanup runs"
    );
}

#[cfg(unix)]
#[test]
fn private_at_file_unsafe_permissions_are_cleaned_before_error() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");
    let content = b"private plan";
    let digest = format!("{:x}", Sha256::digest(content));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    std::fs::write(&source, content).expect("write private plan");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644))
        .expect("make private plan unsafe");
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("unsafe mode rejected");

    assert!(error.message.contains("private input cleanup succeeded"));
    assert!(!source.exists(), "unsafe plaintext is cleaned");
}

#[cfg(unix)]
#[test]
fn private_at_file_stat_failure_reports_cleanup_result() {
    let directory = tempfile::tempdir().expect("private file directory");
    let digest = format!("{:x}", Sha256::digest(b"private plan"));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("missing input rejected");

    assert!(error.message.contains("stat private runner @file"));
    assert!(error.message.contains("private input cleanup succeeded"));
}

#[cfg(unix)]
#[test]
fn private_at_file_read_failure_is_cleaned_before_error() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");
    let content = b"private plan";
    let digest = format!("{:x}", Sha256::digest(content));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    std::fs::write(&source, content).expect("write private plan");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000))
        .expect("make private plan unreadable");
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("unreadable input rejected");

    assert!(error.message.contains("read private runner @file"));
    assert!(error.message.contains("private input cleanup succeeded"));
    assert!(!source.exists(), "unreadable plaintext is cleaned");
}

#[cfg(unix)]
#[test]
fn private_at_file_snapshot_is_atomically_published_with_owner_only_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");

    let snapshot = write_private_at_file_snapshot(directory.path(), b"complete verified content")
        .expect("write snapshot");

    assert_eq!(
        std::fs::read(&snapshot).expect("read snapshot"),
        b"complete verified content"
    );
    assert_eq!(
        std::fs::metadata(&snapshot)
            .expect("snapshot metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let entries = std::fs::read_dir(directory.path())
        .expect("list snapshot directory")
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    assert!(entries.iter().all(|entry| !entry.ends_with(".tmp")));
}

fn private_at_file_request(path: &std::path::Path) -> RemoteRunnerJobRequest {
    RemoteRunnerJobRequest {
        runner_id: "lab".to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: vec!["sh".to_string(), format!("@{}", path.display())],
        cwd: Some("/tmp".to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
        path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        workspace_claim_binding: None,
        workspace_owner_lease: None,
        metadata: None,
    }
}

#[cfg(not(unix))]
#[test]
fn private_at_file_is_rejected_without_unix_filesystem_guarantees() {
    let directory = tempfile::tempdir().expect("private file directory");
    let source = directory.path().join(
        "private-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-plan.json",
    );
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("private input rejected");

    assert!(error
        .message
        .contains("require Unix owner-only filesystem guarantees"));
}
