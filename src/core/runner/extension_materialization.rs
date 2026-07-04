use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleRecord,
    ResourceLifecycleResourceStatus,
};

use super::{
    copy_snapshot_to_directory, exec, Runner, RunnerExecOptions, RunnerFileTransfer, RunnerKind,
};

#[derive(Debug, Clone)]
pub(crate) struct RunnerExtensionMaterializationRequest {
    pub(crate) id: String,
    pub(crate) revision: String,
    pub(crate) source: RunnerExtensionMaterializationSource,
}

#[derive(Debug, Clone)]
pub(crate) enum RunnerExtensionMaterializationSource {
    RemoteGit { url: String, git_ref: String },
    ControllerSnapshot { local_path: PathBuf },
    RunnerPath { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExtensionMaterializationProvenance {
    pub id: String,
    pub source_path: String,
    pub synced_source_path: String,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_fingerprint: Option<String>,
    pub synced_at: String,
    pub dev_overlay: bool,
    pub lifecycle: ResourceLifecycleRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization_source: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerExtensionMaterializationPlan {
    pub id: String,
    pub source_path: String,
    pub synced_source_path: String,
    pub content_hash: String,
}

pub(crate) fn plan_controller_snapshot_extension(
    workspace_root: &str,
    id: &str,
    local_path: &Path,
) -> Result<RunnerExtensionMaterializationPlan> {
    let source_path = expand_path(&local_path.display().to_string());
    let content_hash = extension_source_content_hash(&source_path)?;
    Ok(RunnerExtensionMaterializationPlan {
        id: id.to_string(),
        source_path: source_path.display().to_string(),
        synced_source_path: dev_extension_path(workspace_root, id, &content_hash),
        content_hash,
    })
}

pub(crate) fn materialize_runner_extension(
    runner: &Runner,
    homeboy_path: &str,
    request: &RunnerExtensionMaterializationRequest,
) -> Result<RunnerExtensionMaterializationProvenance> {
    materialize_runner_extension_with_exec(
        runner,
        homeboy_path,
        request,
        &mut |runner_id, options| exec(runner_id, options),
    )
}

pub(crate) fn materialize_runner_extension_with_exec(
    runner: &Runner,
    homeboy_path: &str,
    request: &RunnerExtensionMaterializationRequest,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> Result<RunnerExtensionMaterializationProvenance> {
    match &request.source {
        RunnerExtensionMaterializationSource::ControllerSnapshot { local_path } => {
            let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace_root",
                    "runner extension snapshot materialization requires runner.workspace_root",
                    Some(runner.id.clone()),
                    None,
                )
            })?;
            let plan = plan_controller_snapshot_extension(workspace_root, &request.id, local_path)?;
            sync_controller_snapshot(runner, &plan)?;
            let command = vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "refresh".to_string(),
                plan.synced_source_path.clone(),
                "--id".to_string(),
                request.id.clone(),
                "--ref".to_string(),
                plan.content_hash.clone(),
            ];
            run_materialization_command(runner, command, exec_runner)?;
            Ok(controller_snapshot_provenance(&runner.id, &plan))
        }
        RunnerExtensionMaterializationSource::RunnerPath { path } => {
            let command = vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "refresh".to_string(),
                path.clone(),
                "--id".to_string(),
                request.id.clone(),
                "--ref".to_string(),
                request.revision.clone(),
            ];
            run_materialization_command(runner, command, exec_runner)?;
            Ok(runner_path_provenance(&runner.id, request, path))
        }
        RunnerExtensionMaterializationSource::RemoteGit { url, git_ref } => {
            let exists = runner_extension_exists(runner, homeboy_path, &request.id, exec_runner)
                .map_err(|detail| {
                    Error::validation_invalid_argument(
                        "extensions",
                        detail,
                        Some(request.id.clone()),
                        None,
                    )
                })?;
            let mut command = vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "install".to_string(),
                url.clone(),
                "--id".to_string(),
                request.id.clone(),
                "--ref".to_string(),
                git_ref.clone(),
            ];
            if exists {
                command.push("--replace".to_string());
            }
            run_materialization_command(runner, command, exec_runner)?;
            Ok(remote_git_provenance(
                &runner.id, request, url, git_ref, exists,
            ))
        }
    }
}

fn sync_controller_snapshot(
    runner: &Runner,
    plan: &RunnerExtensionMaterializationPlan,
) -> Result<()> {
    match runner.kind {
        RunnerKind::Local => copy_snapshot_to_directory(
            Path::new(&plan.source_path),
            Path::new(&plan.synced_source_path),
            &[],
        ),
        RunnerKind::Ssh => {
            let transfer = RunnerFileTransfer::for_runner(runner, None)?;
            upload_snapshot(runner, plan, &transfer)
        }
    }
}

fn upload_snapshot(
    runner: &Runner,
    plan: &RunnerExtensionMaterializationPlan,
    transfer: &RunnerFileTransfer,
) -> Result<()> {
    let tempdir = tempfile::tempdir().map_err(|err| {
        Error::internal_io(err.to_string(), Some("stage extension overlay".to_string()))
    })?;
    let staged = tempdir.path().join("source");
    copy_snapshot_to_directory(Path::new(&plan.source_path), &staged, &[])?;
    let archive = tempdir.path().join("source.tar");
    let status = Command::new("tar")
        .args([
            "-C",
            &staged.display().to_string(),
            "-cf",
            &archive.display().to_string(),
            ".",
        ])
        .status()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("archive extension overlay".to_string()),
            )
        })?;
    if !status.success() {
        return Err(Error::internal_unexpected(
            "archive extension overlay failed".to_string(),
        ));
    }

    let remote_archive = format!("{}.tar", plan.synced_source_path.trim_end_matches('/'));
    let remote_parent = Path::new(&plan.synced_source_path)
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| {
            Error::internal_json("invalid remote extension overlay path".to_string(), None)
        })?;
    transfer.ensure_directory(remote_parent)?;
    transfer.upload_file(&archive.display().to_string(), &remote_archive)?;
    let extract = format!(
        "set -e\nrm -rf {dest}\nmkdir -p {dest}\ntar -xf {archive} -C {dest}\nrm -f {archive}\n",
        dest = shell::quote_path(&plan.synced_source_path),
        archive = shell::quote_path(&remote_archive),
    );
    let (_output, exit_code) = exec(
        &runner.id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: true,
            command: vec!["bash".to_string(), "-lc".to_string(), extract],
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            env_materialization: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            path_materialization_plan: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;
    if exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "extensions",
            format!(
                "failed to unpack extension snapshot '{}' on runner",
                plan.id
            ),
            Some(plan.synced_source_path.clone()),
            None,
        ));
    }
    Ok(())
}

fn run_materialization_command(
    runner: &Runner,
    command: Vec<String>,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> Result<()> {
    let result = exec_runner(&runner.id, runner_exec_options(runner, command.clone()));
    match result {
        Ok((_output, 0)) => Ok(()),
        Ok((output, exit_code)) => Err(Error::validation_invalid_argument(
            "extensions",
            format!(
                "runner extension materialization failed with exit code {exit_code}: {}",
                runner_exec_detail(&output)
            ),
            Some(shell_command(&command)),
            None,
        )),
        Err(err) => Err(Error::validation_invalid_argument(
            "extensions",
            format!("runner extension materialization failed: {}", err.message),
            Some(shell_command(&command)),
            None,
        )),
    }
}

pub(crate) fn runner_extension_exists(
    runner: &Runner,
    homeboy_path: &str,
    extension_id: &str,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> std::result::Result<bool, String> {
    let result = exec_runner(
        &runner.id,
        runner_exec_options(
            runner,
            vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "show".to_string(),
                extension_id.to_string(),
            ],
        ),
    );

    match result {
        Ok((_output, 0)) => Ok(true),
        Ok((_output, 4)) => Ok(false),
        Ok((output, exit_code)) => Err(format!(
            "extension {} lookup failed with exit code {}: {}",
            extension_id,
            exit_code,
            runner_exec_detail(&output)
        )),
        Err(err) => Err(format!(
            "extension {} lookup failed: {}",
            extension_id, err.message
        )),
    }
}

fn runner_exec_options(runner: &Runner, command: Vec<String>) -> RunnerExecOptions {
    RunnerExecOptions {
        cwd: None,
        project_id: None,
        allow_diagnostic_ssh: true,
        command,
        env: runner.env.clone(),
        secret_env_names: Vec::new(),
        secret_env_plan: None,
        env_materialization: None,
        capture_patch: false,
        raw_exec: true,
        source_snapshot: None,
        path_materialization_plan: None,
        capability_preflight: None,
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
        runner_workload: None,
        run_id: None,
        detach_after_handoff: false,
        mirror_evidence: true,
        print_handoff: true,
    }
}

fn runner_exec_detail(output: &super::RunnerExecOutput) -> String {
    if !output.stderr.trim().is_empty() {
        output.stderr.trim().to_string()
    } else if !output.stdout.trim().is_empty() {
        output.stdout.trim().to_string()
    } else {
        "no runner output".to_string()
    }
}

fn controller_snapshot_provenance(
    runner_id: &str,
    plan: &RunnerExtensionMaterializationPlan,
) -> RunnerExtensionMaterializationProvenance {
    let source = Path::new(&plan.source_path);
    let dirty = git_dirty(source);
    RunnerExtensionMaterializationProvenance {
        id: plan.id.clone(),
        source_path: plan.source_path.clone(),
        synced_source_path: plan.synced_source_path.clone(),
        content_hash: plan.content_hash.clone(),
        source_revision: git_revision(source),
        dirty,
        dirty_fingerprint: dirty.then(|| git_dirty_fingerprint(source)).flatten(),
        synced_at: chrono::Utc::now().to_rfc3339(),
        dev_overlay: true,
        lifecycle: dev_extension_lifecycle(runner_id, &plan.synced_source_path, &plan.id),
        materialization_source: Some(serde_json::json!({
            "kind": "controller_snapshot",
            "local_path": plan.source_path,
            "content_hash": plan.content_hash,
        })),
    }
}

fn runner_path_provenance(
    runner_id: &str,
    request: &RunnerExtensionMaterializationRequest,
    path: &str,
) -> RunnerExtensionMaterializationProvenance {
    RunnerExtensionMaterializationProvenance {
        id: request.id.clone(),
        source_path: path.to_string(),
        synced_source_path: path.to_string(),
        content_hash: request.revision.clone(),
        source_revision: Some(request.revision.clone()),
        dirty: false,
        dirty_fingerprint: None,
        synced_at: chrono::Utc::now().to_rfc3339(),
        dev_overlay: false,
        lifecycle: dev_extension_lifecycle(runner_id, path, &request.id),
        materialization_source: Some(serde_json::json!({
            "kind": "runner_path",
            "path": path,
        })),
    }
}

fn remote_git_provenance(
    runner_id: &str,
    request: &RunnerExtensionMaterializationRequest,
    url: &str,
    git_ref: &str,
    replaced: bool,
) -> RunnerExtensionMaterializationProvenance {
    RunnerExtensionMaterializationProvenance {
        id: request.id.clone(),
        source_path: url.to_string(),
        synced_source_path: url.to_string(),
        content_hash: request.revision.clone(),
        source_revision: Some(request.revision.clone()),
        dirty: false,
        dirty_fingerprint: None,
        synced_at: chrono::Utc::now().to_rfc3339(),
        dev_overlay: false,
        lifecycle: dev_extension_lifecycle(runner_id, url, &request.id),
        materialization_source: Some(serde_json::json!({
            "kind": "remote_git",
            "url": url,
            "ref": git_ref,
            "replaced": replaced,
        })),
    }
}

pub(crate) fn dev_extension_path(workspace_root: &str, id: &str, content_hash: &str) -> String {
    format!(
        "{}/_lab_workspaces/dev-extensions/{}/{}/",
        workspace_root.trim_end_matches('/'),
        sanitize_ref(id),
        &content_hash[..16]
    )
}

pub(crate) fn extension_source_content_hash(path: &Path) -> Result<String> {
    let mut entries = Vec::new();
    collect_hash_entries(path, path, &mut entries)?;
    entries.sort();
    let mut hasher = Sha256::new();
    for (relative, path) in entries {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let mut file = fs::File::open(&path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|err| Error::internal_json(err.to_string(), None))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_hash_entries(
    root: &Path,
    path: &Path,
    entries: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let mut children = fs::read_dir(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    children.sort_by_key(|entry| entry.path());
    for entry in children {
        let path = entry.path();
        if entry.file_name().to_string_lossy() == ".git" {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        if metadata.is_dir() {
            collect_hash_entries(root, &path, entries)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            entries.push((relative, path));
        }
    }
    Ok(())
}

pub(crate) fn dev_extension_lifecycle(
    runner_id: &str,
    remote_path: &str,
    extension_id: &str,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: "runner.dev_sync.extension_overlay".to_string(),
        run_id: format!("runner-dev-sync-{runner_id}-{extension_id}"),
        runner_id: Some(runner_id.to_string()),
        path: remote_path.to_string(),
        root_bound: None,
        kind: "runner_dev_extension_overlay".to_string(),
        ttl: Some("P7D".to_string()),
        cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: Default::default(),
        cleanup_command: Some(format!(
            "homeboy runner dev-sync {} --extensions {}=<path>",
            shell_arg(runner_id),
            shell_arg(extension_id)
        )),
        status: ResourceLifecycleResourceStatus::Active,
    }
}

pub(crate) fn expand_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).to_string())
}

pub(crate) fn git_revision(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", &path.display().to_string(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn git_dirty(path: &Path) -> bool {
    Command::new("git")
        .args(["-C", &path.display().to_string(), "status", "--porcelain"])
        .output()
        .ok()
        .is_some_and(|output| !output.stdout.is_empty())
}

pub(crate) fn git_dirty_fingerprint(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args([
            "-C",
            &path.display().to_string(),
            "status",
            "--porcelain=v1",
        ])
        .output()
        .ok()?;
    output.status.success().then(|| {
        let mut hasher = Sha256::new();
        hasher.update(&output.stdout);
        format!("{:x}", hasher.finalize())
    })
}

fn sanitize_ref(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    sanitized.trim_matches('-').to_string().if_empty("main")
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn shell_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| shell::quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_arg(value: &str) -> String {
    shell::quote_path(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::Runner;

    fn runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some("/tmp/homeboy-runner-ws".to_string()),
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        }
    }

    fn output() -> super::super::RunnerExecOutput {
        super::super::RunnerExecOutput {
            variant: "runner_exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: super::super::RunnerExecMode::Local,
            argv: Vec::new(),
            remote_cwd: String::new(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        }
    }

    #[test]
    fn remote_git_materialization_installs_or_replaces_by_runner_state() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RemoteGit {
                url: "https://example.test/extensions.git".to_string(),
                git_ref: "abc123".to_string(),
            },
        };
        let mut commands = Vec::new();
        let provenance = materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            &request,
            &mut |_runner_id, options| {
                commands.push(options.command.clone());
                Ok((output(), if commands.len() == 1 { 0 } else { 0 }))
            },
        )
        .expect("materializes");

        assert_eq!(commands[0], vec!["homeboy", "extension", "show", "rust"]);
        assert_eq!(
            commands[1][..4],
            [
                "homeboy",
                "extension",
                "install",
                "https://example.test/extensions.git"
            ]
        );
        assert!(commands[1].contains(&"--replace".to_string()));
        assert_eq!(
            provenance.materialization_source.as_ref().unwrap()["kind"],
            "remote_git"
        );
    }

    #[test]
    fn runner_path_materialization_refreshes_path() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RunnerPath {
                path: "/runner/extensions/rust".to_string(),
            },
        };
        let mut command = Vec::new();
        materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            &request,
            &mut |_runner_id, options| {
                command = options.command;
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(
            command,
            vec![
                "homeboy",
                "extension",
                "refresh",
                "/runner/extensions/rust",
                "--id",
                "rust",
                "--ref",
                "abc123"
            ]
        );
    }

    #[test]
    fn controller_snapshot_plan_uses_content_hash_slot() {
        let dir = tempfile::tempdir().expect("extension source");
        std::fs::write(dir.path().join("rust.json"), r#"{"id":"rust"}"#).expect("manifest");
        std::fs::write(dir.path().join("run.sh"), "echo hi\n").expect("source");

        let plan = plan_controller_snapshot_extension("/runner/ws/", "rust", dir.path())
            .expect("snapshot plan");

        assert_eq!(plan.id, "rust");
        assert!(plan
            .synced_source_path
            .starts_with("/runner/ws/_lab_workspaces/dev-extensions/rust/"));
        assert!(plan.synced_source_path.ends_with('/'));
    }
}
