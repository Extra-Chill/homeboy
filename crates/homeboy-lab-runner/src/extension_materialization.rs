use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleRecord,
    ResourceLifecycleResourceStatus,
};

use super::{
    copy_snapshot_to_directory, exec, Runner, RunnerExecOptions, RunnerFileTransfer, RunnerKind,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunnerExtensionMaterializationRequest {
    pub(crate) id: String,
    pub(crate) revision: String,
    pub(crate) source: RunnerExtensionMaterializationSource,
}

#[derive(Debug, Clone, Serialize)]
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
    materialize_runner_extension_with_env(runner, homeboy_path, None, request)
}

pub(crate) fn materialize_runner_extension_with_env(
    runner: &Runner,
    homeboy_path: &str,
    env: Option<HashMap<String, String>>,
    request: &RunnerExtensionMaterializationRequest,
) -> Result<RunnerExtensionMaterializationProvenance> {
    materialize_runner_extension_with_exec(
        runner,
        homeboy_path,
        env,
        request,
        &mut |runner_id, options| exec(runner_id, options),
    )
}

pub(crate) fn materialize_runner_extension_with_exec(
    runner: &Runner,
    homeboy_path: &str,
    env: Option<HashMap<String, String>>,
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
            let exists = runner_extension_exists(
                runner,
                env.as_ref(),
                homeboy_path,
                &request.id,
                exec_runner,
            )
            .map_err(|detail| {
                Error::validation_invalid_argument(
                    "extensions",
                    detail,
                    Some(request.id.clone()),
                    None,
                )
            })?;
            let command = local_path_install_command(
                homeboy_path,
                &plan.synced_source_path,
                &request.id,
                &plan.content_hash,
                exists,
            );
            run_materialization_command(runner, env.as_ref(), command, exec_runner)?;
            Ok(controller_snapshot_provenance(&runner.id, &plan))
        }
        RunnerExtensionMaterializationSource::RunnerPath { path } => {
            let exists = runner_extension_exists(
                runner,
                env.as_ref(),
                homeboy_path,
                &request.id,
                exec_runner,
            )
            .map_err(|detail| {
                Error::validation_invalid_argument(
                    "extensions",
                    detail,
                    Some(request.id.clone()),
                    None,
                )
            })?;
            let command = local_path_install_command(
                homeboy_path,
                path,
                &request.id,
                &request.revision,
                exists,
            );
            run_materialization_command(runner, env.as_ref(), command, exec_runner)?;
            Ok(runner_path_provenance(&runner.id, request, path))
        }
        RunnerExtensionMaterializationSource::RemoteGit { url, git_ref } => {
            let exists = runner_extension_exists(
                runner,
                env.as_ref(),
                homeboy_path,
                &request.id,
                exec_runner,
            )
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
            run_materialization_command(runner, env.as_ref(), command, exec_runner)?;
            Ok(remote_git_provenance(
                &runner.id, request, url, git_ref, exists,
            ))
        }
    }
}

fn local_path_install_command(
    homeboy_path: &str,
    source_path: &str,
    extension_id: &str,
    revision: &str,
    replace: bool,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source_path.to_string(),
        "--id".to_string(),
        extension_id.to_string(),
        "--ref".to_string(),
        revision.to_string(),
    ];
    if replace {
        command.push("--replace".to_string());
    }
    command
}

fn sync_controller_snapshot(
    runner: &Runner,
    plan: &RunnerExtensionMaterializationPlan,
) -> Result<()> {
    let synced_source_path = plan.synced_source_path.trim_end_matches('/');
    match runner.kind {
        RunnerKind::Local => copy_snapshot_to_directory(
            Path::new(&plan.source_path),
            Path::new(synced_source_path),
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
    let (_output, exit_code) = exec(&runner.id, RunnerExecOptions::diagnostic_raw_shell(extract))?;
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
    env: Option<&HashMap<String, String>>,
    command: Vec<String>,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> Result<()> {
    let result = exec_runner(
        &runner.id,
        runner_exec_options(runner, env, command.clone()),
    );
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
    env: Option<&HashMap<String, String>>,
    homeboy_path: &str,
    extension_id: &str,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> std::result::Result<bool, String> {
    let result = exec_runner(
        &runner.id,
        runner_exec_options(
            runner,
            env,
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

fn runner_exec_options(
    runner: &Runner,
    env: Option<&HashMap<String, String>>,
    command: Vec<String>,
) -> RunnerExecOptions {
    RunnerExecOptions::diagnostic_raw_command(command)
        .with_env(env.cloned().unwrap_or_else(|| runner.env.clone()))
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
        lifecycle: installed_extension_lifecycle(
            runner_id,
            path,
            &request.id,
            "linked_runner_path",
        ),
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
        lifecycle: installed_extension_lifecycle(
            runner_id,
            url,
            &request.id,
            "remote_git_checkout",
        ),
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

fn installed_extension_lifecycle(
    runner_id: &str,
    source: &str,
    extension_id: &str,
    source_kind: &str,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: format!("runner.extension.{source_kind}"),
        run_id: format!("runner-extension-{runner_id}-{extension_id}"),
        runner_id: Some(runner_id.to_string()),
        path: source.to_string(),
        root_bound: None,
        kind: "runner_installed_extension".to_string(),
        ttl: None,
        cleanup_policy: ResourceCleanupPolicy::Preserve,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: Default::default(),
        cleanup_command: None,
        status: ResourceLifecycleResourceStatus::Retained,
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
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "main".to_string()
    } else {
        sanitized.to_string()
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
    use crate::Runner;

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
            None,
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
        assert_eq!(
            provenance.lifecycle.owner,
            "runner.extension.remote_git_checkout"
        );
        assert_eq!(
            provenance.lifecycle.cleanup_policy,
            ResourceCleanupPolicy::Preserve
        );
    }

    #[test]
    fn runner_path_materialization_links_active_path() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RunnerPath {
                path: "/runner/extensions/rust".to_string(),
            },
        };
        let mut commands = Vec::new();
        materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            None,
            &request,
            &mut |_runner_id, options| {
                commands.push(options.command);
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(commands[0], vec!["homeboy", "extension", "show", "rust"]);
        assert_eq!(
            commands[1],
            vec![
                "homeboy",
                "extension",
                "install",
                "/runner/extensions/rust",
                "--id",
                "rust",
                "--ref",
                "abc123",
                "--replace"
            ]
        );
        let provenance = runner_path_provenance(&runner.id, &request, "/runner/extensions/rust");
        assert_eq!(
            provenance.lifecycle.owner,
            "runner.extension.linked_runner_path"
        );
        assert_eq!(
            provenance.lifecycle.status,
            ResourceLifecycleResourceStatus::Retained
        );
    }

    #[test]
    fn controller_snapshot_materialization_links_synced_dev_overlay() {
        let workspace = tempfile::tempdir().expect("runner workspace");
        let mut runner = runner();
        runner.workspace_root = Some(workspace.path().to_string_lossy().to_string());
        let dir = tempfile::tempdir().expect("extension source");
        std::fs::write(dir.path().join("nodejs.json"), r#"{"id":"nodejs"}"#).expect("manifest");
        std::fs::write(dir.path().join("bench-runner.sh"), "echo bench\n").expect("script");
        let request = RunnerExtensionMaterializationRequest {
            id: "nodejs".to_string(),
            revision: "24ebb9fdaa53c3387669ec3222a5cb7fe26e8884".to_string(),
            source: RunnerExtensionMaterializationSource::ControllerSnapshot {
                local_path: dir.path().to_path_buf(),
            },
        };
        let mut commands = Vec::new();

        let provenance = materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            None,
            &request,
            &mut |_runner_id, options| {
                commands.push(options.command);
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(commands[0], vec!["homeboy", "extension", "show", "nodejs"]);
        assert_eq!(commands[1][..3], ["homeboy", "extension", "install"]);
        assert_eq!(commands[1][4], "--id");
        assert_eq!(commands[1][5], "nodejs");
        assert_eq!(commands[1][6], "--ref");
        assert_eq!(commands[1][8], "--replace");
        assert!(commands[1][3].starts_with(&format!(
            "{}/_lab_workspaces/dev-extensions/nodejs/",
            workspace.path().display()
        )));
        assert!(provenance.dev_overlay);
        assert!(provenance.synced_source_path.starts_with(&format!(
            "{}/_lab_workspaces/dev-extensions/nodejs/",
            workspace.path().display()
        )));
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

    #[test]
    fn materialization_can_use_explicit_command_env() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RunnerPath {
                path: "/runner/extensions/rust".to_string(),
            },
        };
        let mut env = std::collections::HashMap::new();
        env.insert("PATH".to_string(), "/usr/local/bin:/usr/bin".to_string());
        let mut captured_env = std::collections::HashMap::new();

        materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            Some(env),
            &request,
            &mut |_runner_id, options| {
                captured_env = options.env;
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(
            captured_env.get("PATH").map(String::as_str),
            Some("/usr/local/bin:/usr/bin")
        );
        assert_eq!(captured_env.get("HOMEBOY_COMMAND"), None);
    }
}
