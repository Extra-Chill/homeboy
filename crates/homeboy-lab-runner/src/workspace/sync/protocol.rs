//! Stage-wide, two-call source materialization. No runner effects occur while
//! preparing the controller's immutable transfer.
use super::*;
pub(super) use homeboy_lab_contract::LAB_CONTRACT_VERSION as CONTRACT;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Serialize, Deserialize)]
pub(super) struct PlanRequest {
    pub(super) lab_contract_version: u32,
    pub(super) roots: Vec<RootSummary>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct RootSummary {
    pub(super) remote_path: String,
    pub(super) object_cache: String,
    pub(super) manifest: super::super::snapshot::WorkspaceContentManifest,
    pub(super) content_hash: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct PlanResponse {
    pub(super) lab_contract_version: u32,
    pub(super) available_bytes: u64,
    pub(super) available_inodes: u64,
    pub(super) total_bytes: u64,
    pub(super) tips: Vec<Vec<String>>,
    pub(super) missing: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct CommitRequest {
    pub(super) lab_contract_version: u32,
    pub(super) roots: Vec<CommitRoot>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct CommitRoot {
    pub(super) summary: RootSummary,
    pub(super) metadata: RunnerWorkspaceMetadata,
    pub(super) install_git: Option<GitInstall>,
    pub(super) prepared_git: bool,
    pub(super) source_evidence: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct GitInstall {
    pub(super) head: String,
    pub(super) branch: Option<String>,
    pub(super) remote_url: String,
    pub(super) sha256: String,
    pub(super) prerequisites: Vec<String>,
    pub(super) retain: Vec<String>,
}

fn materialization_error(mut error: Error, input: &Path) -> Error {
    if error.is_storage_exhausted() {
        error.retryable = Some(true);
        error.details["runner_path"] = serde_json::json!(input);
    }
    error
}

/// Keep the filesystem boundary in both the human message and structured
/// evidence. Callers supply the owning side, never infer it from the host.
pub(super) trait MaterializationIo<T> {
    fn materialization(self, operation: &str, path: &Path, side: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> MaterializationIo<T> for std::result::Result<T, E> {
    fn materialization(self, operation: &str, path: &Path, side: &str) -> Result<T> {
        self.map_err(|source| {
            let context = format!("{side} materialization");
            let mut error = Error::internal_io(source.to_string(), Some(context.clone()));
            error.message = format!(
                "{}: {operation} {}: {source} ({context})",
                error.message,
                path.display()
            );
            error.details["path"] = serde_json::json!(path);
            error.details["operation"] = serde_json::json!(operation);
            error.details["side"] = serde_json::json!(side);
            error.retryable = Some(true);
            error.details["gate_disposition"] = serde_json::json!("unavailable");
            error
        })
    }
}

pub(super) fn io(error: impl std::fmt::Display) -> Error {
    Error::internal_io(
        error.to_string(),
        Some("workspace materialization protocol".into()),
    )
}

pub(super) fn json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|error| Error::internal_json(error.to_string(), None))
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec(value).map_err(io)?).materialization(
        "write",
        path,
        "controller",
    )
}

/// Runner-side plan endpoint. Its response is the sole source of admission and
/// object-cache evidence; it never creates a workspace.
pub fn materialization_plan(input: &Path) -> Result<serde_json::Value> {
    materialization_plan_controlled(input, &WorkspaceControl::default())
        .map_err(|error| materialization_error(error, input))
}

pub(super) fn materialization_plan_controlled(
    input: &Path,
    control: &WorkspaceControl,
) -> Result<serde_json::Value> {
    control.checkpoint()?;
    let request: PlanRequest = json(&fs::read(input).materialization("read", input, "runner")?)?;
    let version = request.lab_contract_version;
    if version != CONTRACT {
        return Err(io(format!(
            "Lab contract version mismatch: received {version}, expected {CONTRACT}"
        )));
    }
    let first = request
        .roots
        .first()
        .ok_or_else(|| io("empty materialization plan"))?;
    let store_root = super::blobs::store(&first.remote_path);
    let _scratch = crate::staging_temp::Scope::new(store_root.parent().unwrap());
    let staging = store_root.parent().unwrap().join(".homeboy-tmp");
    fs::create_dir_all(&staging).materialization("create_dir_all", &staging, "runner")?;
    let probe = local_snapshot_filesystem_probe(&staging, "runner snapshot destination")?;
    let disk = local_runner_workspace_disk_probe(&staging)
        .ok_or_else(|| io("runner filesystem capacity unavailable"))?;
    let store = super::blobs::store(&first.remote_path);
    super::blobs::prune(&store)?;
    let inventory = super::blobs::inventory(&request.roots)?;
    let mut missing = Vec::new();
    let mut required_bytes = 0u64;
    for (digest, bytes) in &inventory {
        let path = store.join(super::blobs::key(digest)?);
        // Refreshing a reused blob's mtime keeps concurrent age pruning off it
        // until this materialization assembles it.
        let reusable = super::blobs::valid(&path, digest, control)?
            && fs::File::open(&path)
                .and_then(|file| file.set_modified(std::time::SystemTime::now()))
                .is_ok();
        if !reusable {
            let _ = fs::remove_file(path);
            missing.push(digest.clone());
            required_bytes = required_bytes.saturating_add(*bytes);
        }
    }
    let mut required_inodes = missing.len() as u64;
    for root in &request.roots {
        let size = super::blobs::assembled_size(root);
        required_bytes = required_bytes.saturating_add(size.bytes);
        required_inodes += size.inodes;
    }
    if required_bytes > probe.available_bytes
        || required_inodes > probe.available_inodes
        || runner_workspace_disk_is_critical(disk)
    {
        return Err(io("runner snapshot capacity refused by plan"));
    }
    let mut tips = Vec::new();
    for root in request.roots {
        let output = homeboy_core::observation::phase_timing::time(
            "stage.lab.source_materialization.runner_inventory",
            || {
                control.output(
                    Command::new("git").args([
                        "-C",
                        &root.object_cache,
                        "for-each-ref",
                        "--sort=-committerdate",
                        "--count=64",
                        "--format=%(objectname)",
                        "refs/homeboy/",
                    ]),
                    "plan runner object-cache inventory",
                )
            },
        )?;
        tips.push(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.len() >= 40 && line.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_string)
                .collect(),
        );
    }
    control.checkpoint()?;
    serde_json::to_value(PlanResponse {
        lab_contract_version: CONTRACT,
        available_bytes: probe.available_bytes,
        available_inodes: probe.available_inodes,
        total_bytes: disk.total_bytes,
        tips,
        missing,
    })
    .map_err(io)
}

/// Runner-side commit endpoint. `input` is an already extracted, private
/// transport directory. Verify every root before publishing any execution view.
pub fn materialization_commit(input: &Path) -> Result<serde_json::Value> {
    materialization_commit_controlled(input, &WorkspaceControl::default())
        .map_err(|error| materialization_error(error, input))
}

pub(super) fn materialization_commit_controlled(
    input: &Path,
    control: &WorkspaceControl,
) -> Result<serde_json::Value> {
    control.checkpoint()?;
    let request: CommitRequest = json(&fs::read(input.join("commit.json")).materialization(
        "read",
        &input.join("commit.json"),
        "runner",
    )?)?;
    let version = request.lab_contract_version;
    if version != CONTRACT {
        return Err(io(format!(
            "Lab contract version mismatch: received {version}, expected {CONTRACT}"
        )));
    }
    let first = request.roots.first().ok_or_else(|| io("empty commit"))?;
    let store = super::blobs::store(&first.summary.remote_path);
    let _scratch = crate::staging_temp::Scope::new(store.parent().unwrap());
    fs::create_dir_all(&store).materialization("create_dir_all", &store, "runner")?;
    if let Ok(blobs) = fs::read_dir(input.join("blobs")) {
        for blob in blobs {
            control.checkpoint()?;
            let blob = blob.materialization("read_dir entry", &input.join("blobs"), "runner")?;
            let digest = format!("sha256:{}", blob.file_name().to_string_lossy());
            let key = super::blobs::key(&digest)?;
            if !super::blobs::valid(&blob.path(), &digest, control)? {
                return Err(io("transferred blob digest mismatch"));
            }
            fs::rename(blob.path(), store.join(key)).materialization(
                "rename",
                &blob.path(),
                "runner",
            )?;
        }
    }
    let mut synthetic = Vec::new();
    for (index, root) in request.roots.iter().enumerate() {
        control.checkpoint()?;
        validate_absolute_path("remote_path", &root.summary.remote_path)?;
        if root.metadata.remote_path != root.summary.remote_path {
            return Err(io("materialization metadata path mismatch"));
        }
        let source = input.join(index.to_string()).join("source");
        super::blobs::assemble(&root.summary, &source, control)?;
        let (actual, digest) =
            super::super::snapshot::workspace_content_manifest_and_hash_unspanned(
                &source,
                &[],
                WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
                control,
            )?;
        if digest != root.summary.content_hash {
            return Err(io("runner materialization content digest mismatch"));
        }
        for (expected, actual) in root.summary.manifest.entries.iter().zip(&actual.entries) {
            if expected.path != actual.path
                || expected.symlink_target != actual.symlink_target
                || (expected.kind == "file"
                    && expected.mode.is_some()
                    && expected.mode != actual.mode)
            {
                return Err(io("runner materialization mode or symlink mismatch"));
            }
        }
        if let Some(git) = &root.install_git {
            let command = super::super::git::git_bundle_install_command(
                "./git",
                &git.head,
                git.branch.as_deref(),
                &git.remote_url,
                None,
                false,
                super::super::git::BundleTransfer {
                    metadata_only: true,
                    sha256: &git.sha256,
                    object_cache: &root.summary.object_cache,
                    prerequisites: &git.prerequisites,
                    retain: &git.retain,
                },
            );
            let bundle = input.join(index.to_string()).join("workspace.bundle");
            let output = control.output_with_stdin(
                Command::new("sh")
                    .args(["-c", &command])
                    .current_dir(input.join(index.to_string())),
                fs::File::open(&bundle).materialization("open", &bundle, "runner")?,
                "install materialization Git metadata",
            )?;
            if !output.status.success() {
                return Err(io(String::from_utf8_lossy(&output.stderr)));
            }
        }
        if root.prepared_git {
            let command = super::super::snapshot::synthetic_git_checkout_command(
                &source.display().to_string(),
                &root.metadata.snapshot_identity,
                None,
                None,
                false,
            );
            control.shell(&command, "install synthetic Git baseline")?;
            synthetic.push(serde_json::json!({
                "synthetic_commit": control.git(&source, &["rev-parse", "HEAD"])?,
                "synthetic_ref": control.git(&source, &["symbolic-ref", "HEAD"])?,
                "synthetic_tree": control.git(&source, &["rev-parse", "HEAD^{tree}"])?,
            }));
        } else {
            synthetic.push(serde_json::Value::Null);
        }
        if root.install_git.is_some() {
            fs::rename(
                input.join(index.to_string()).join("git/.git"),
                source.join(".git"),
            )
            .materialization("rename Git metadata", &source.join(".git"), "runner")?;
            control.git(&source, &["read-tree", "HEAD"])?;
            let head = control.git(&source, &["rev-parse", "--verify", "HEAD"])?;
            if !root.prepared_git && root.metadata.source_commit.as_deref() != Some(head.as_str()) {
                return Err(io("materialization Git HEAD mismatch"));
            }
        }
    }
    for (index, root) in request.roots.into_iter().enumerate() {
        control.checkpoint()?;
        let source = input.join(index.to_string()).join("source");
        let destination = Path::new(&root.summary.remote_path);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| io("workspace has no parent"))?,
        )
        .materialization("create_dir_all", destination.parent().unwrap(), "runner")?;
        // Never replace another job's live execution view. An interrupted
        // publish is accepted only if its exact bytes are already complete.
        if destination.exists() {
            // The review/git sync path publishes prepared dependency siblings
            // with source evidence, not workspace lease metadata. Both paths
            // share these siblings: require a lease only for job-owned roots.
            if root.source_evidence.is_none() {
                let previous: RunnerWorkspaceMetadata = json(
                    &fs::read(workspace_metadata_path(&root.summary.remote_path)).materialization(
                        "read",
                        Path::new(&workspace_metadata_path(&root.summary.remote_path)),
                        "runner",
                    )?,
                )?;
                if previous.workspace_lease != root.metadata.workspace_lease {
                    return Err(io("existing materialization belongs to another lease"));
                }
            }
            if root.install_git.is_some() || root.prepared_git {
                let expected = control.git(&source, &["rev-parse", "HEAD"])?;
                if control.git(destination, &["rev-parse", "HEAD"])? != expected {
                    return Err(io("existing materialization has different Git HEAD"));
                }
            }
            let (_, digest) =
                super::super::snapshot::workspace_content_manifest_and_hash_unspanned(
                    destination,
                    &root.metadata.snapshot_excludes,
                    WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
                    control,
                )?;
            if digest != root.summary.content_hash {
                if root.source_evidence.is_none() {
                    return Err(io("existing materialization has different content"));
                }
                // Legacy dependency siblings are mutable prepared views, not
                // job-owned source roots. Retire the old complete view only
                // after its replacement has passed verification.
                let retired = input.join(format!("retired-{index}"));
                fs::rename(destination, retired).materialization(
                    "rename",
                    destination,
                    "runner",
                )?;
            }
        }
        homeboy_core::observation::phase_timing::time(
            "stage.lab.source_materialization.record_writes",
            || -> Result<()> {
                let metadata_path =
                    PathBuf::from(workspace_metadata_path(&root.summary.remote_path));
                fs::create_dir_all(
                    metadata_path
                        .parent()
                        .ok_or_else(|| io("metadata has no parent"))?,
                )
                .materialization(
                    "create_dir_all",
                    metadata_path.parent().unwrap(),
                    "runner",
                )?;
                let mut staged = tempfile::NamedTempFile::new_in(metadata_path.parent().unwrap())
                    .materialization("create tempfile", &metadata_path, "runner")?;
                staged
                    .write_all(&serde_json::to_vec(&root.metadata).map_err(io)?)
                    .materialization("write", &metadata_path, "runner")?;
                staged.persist(&metadata_path).materialization(
                    "persist",
                    &metadata_path,
                    "runner",
                )?;
                if let Some(evidence) = &root.source_evidence {
                    let path = homeboy_lab_contract::lab::handoff::runner_workspace_lab_source_evidence_path(&root.summary.remote_path);
                    fs::write(&path, evidence).materialization(
                        "write",
                        Path::new(&path),
                        "runner",
                    )?;
                }
                Ok(())
            },
        )?;
        // Metadata is complete before the workspace becomes visible. Recovery
        // can finish a publish interrupted between these two atomic renames.
        if !destination.exists() {
            fs::rename(&source, destination).materialization("rename", &source, "runner")?;
        }
    }
    control.checkpoint()?;
    super::blobs::prune(&store)?;
    Ok(serde_json::json!({"committed": true, "synthetic": synthetic}))
}
