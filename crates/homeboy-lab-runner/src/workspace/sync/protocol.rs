//! Stage-wide, two-call source materialization. No runner effects occur while
//! preparing the controller's immutable transfer.
use super::*;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Serialize, Deserialize)]
pub(super) struct PlanRequest {
    pub(super) identity: String,
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
    pub(super) identity: String,
    pub(super) available_bytes: u64,
    pub(super) available_inodes: u64,
    pub(super) total_bytes: u64,
    pub(super) tips: Vec<Vec<String>>,
    pub(super) missing: Vec<Vec<super::super::snapshot::WorkspaceContentManifestEntry>>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct CommitRequest {
    pub(super) identity: String,
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
    fs::write(path, serde_json::to_vec(value).map_err(io)?).map_err(io)
}

/// Runner-side plan endpoint. Its response is the sole source of admission and
/// object-cache evidence; it never creates a workspace.
pub fn materialization_plan(input: &Path) -> Result<serde_json::Value> {
    materialization_plan_controlled(input, &WorkspaceControl::default())
}

pub(super) fn materialization_plan_controlled(
    input: &Path,
    control: &WorkspaceControl,
) -> Result<serde_json::Value> {
    control.checkpoint()?;
    let request: PlanRequest = json(&fs::read(input).map_err(io)?)?;
    let first = request
        .roots
        .first()
        .ok_or_else(|| io("empty materialization plan"))?;
    let probe = local_snapshot_filesystem_probe(
        Path::new(&first.remote_path),
        "runner snapshot destination",
    )?;
    let disk = local_runner_workspace_disk_probe(Path::new(&first.remote_path))
        .ok_or_else(|| io("runner filesystem capacity unavailable"))?;
    let mut tips = Vec::new();
    let mut missing = Vec::new();
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
        // The execution view is private, not a mutable seed from another job.
        // Until blob-addressed transfer lands, all overlay paths are requested.
        missing.push(root.manifest.entries);
    }
    control.checkpoint()?;
    serde_json::to_value(PlanResponse {
        identity: homeboy_product_identity::build_identity().display,
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
}

pub(super) fn materialization_commit_controlled(
    input: &Path,
    control: &WorkspaceControl,
) -> Result<serde_json::Value> {
    control.checkpoint()?;
    let request: CommitRequest = json(&fs::read(input.join("commit.json")).map_err(io)?)?;
    if request.identity != homeboy_product_identity::build_identity().display {
        return Err(io(
            "runner build identity changed since materialization plan",
        ));
    }
    for (index, root) in request.roots.iter().enumerate() {
        control.checkpoint()?;
        validate_absolute_path("remote_path", &root.summary.remote_path)?;
        if root.metadata.remote_path != root.summary.remote_path {
            return Err(io("materialization metadata path mismatch"));
        }
        let source = input.join(index.to_string()).join("source");
        let (_, digest) = super::super::snapshot::workspace_content_manifest_and_hash_unspanned(
            &source,
            &[],
            WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
            control,
        )?;
        if digest != root.summary.content_hash {
            return Err(io("runner materialization content digest mismatch"));
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
                fs::File::open(bundle).map_err(io)?,
                "install materialization Git metadata",
            )?;
            if !output.status.success() {
                return Err(io(String::from_utf8_lossy(&output.stderr)));
            }
        }
        if root.install_git.is_some() || root.prepared_git {
            fs::rename(
                input.join(index.to_string()).join("git/.git"),
                source.join(".git"),
            )
            .map_err(io)?;
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
        .map_err(io)?;
        // Never replace another job's live execution view. An interrupted
        // publish is accepted only if its exact bytes are already complete.
        if destination.exists() {
            let previous: RunnerWorkspaceMetadata =
                json(&fs::read(workspace_metadata_path(&root.summary.remote_path)).map_err(io)?)?;
            if previous.workspace_lease != root.metadata.workspace_lease
                && root.source_evidence.is_none()
            {
                return Err(io("existing materialization belongs to another lease"));
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
                fs::rename(destination, retired).map_err(io)?;
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
                .map_err(io)?;
                let mut staged =
                    tempfile::NamedTempFile::new_in(metadata_path.parent().unwrap()).map_err(io)?;
                staged
                    .write_all(&serde_json::to_vec(&root.metadata).map_err(io)?)
                    .map_err(io)?;
                staged.persist(metadata_path).map_err(io)?;
                if let Some(evidence) = &root.source_evidence {
                    let path = homeboy_lab_contract::lab::handoff::runner_workspace_lab_source_evidence_path(&root.summary.remote_path);
                    fs::write(path, evidence).map_err(io)?;
                }
                Ok(())
            },
        )?;
        // Metadata is complete before the workspace becomes visible. Recovery
        // can finish a publish interrupted between these two atomic renames.
        if !destination.exists() {
            fs::rename(source, destination).map_err(io)?;
        }
    }
    control.checkpoint()?;
    Ok(serde_json::json!({"committed": true}))
}
