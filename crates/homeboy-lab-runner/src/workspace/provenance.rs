use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy_core::engine::shell;
use homeboy_core::observation::{LAB_OFFLOAD_METADATA_ENV, SOURCE_SNAPSHOT_METADATA_ENV};
use homeboy_core::source_snapshot::SourceSnapshot;

use super::snapshot::{
    workspace_content_hash_for_policy, workspace_content_hash_v1,
    workspace_content_manifest_for_policy, WorkspaceContentManifest, WorkspaceContentManifestEntry,
};
use super::workspace_content_hash_algorithm;

const LAB_SOURCE_SNAPSHOT_SYNC_MODE: &str = "lab_offload";
const SYNTHETIC_SNAPSHOT_BASELINE_REF: &str = "refs/heads/homeboy-snapshot-baseline";
const SYNTHETIC_SNAPSHOT_AUTHOR: &str = "Homeboy Snapshot <snapshot@homeboy.invalid> 0 +0000";
const CONTENT_MANIFEST_DIFFERENCE_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedLabWorkspaceProvenance {
    pub source_revision: String,
    pub materialization_mode: String,
    pub runner_id: String,
    pub workspace_identity: String,
    pub snapshot_hash: String,
    content_hash: String,
    content_hash_algorithm: String,
    permission_policy: Option<String>,
    content_manifest: Option<WorkspaceContentManifest>,
    sync_excludes: Vec<String>,
    local_source_path: Option<String>,
    remote_workspace_path: String,
    synthetic_checkout_commit: Option<String>,
    synthetic_checkout_ref: Option<String>,
    synthetic_checkout_tree: Option<String>,
}

pub(crate) fn materialize_verified_lab_snapshot_git_baseline(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
    snapshot: SourceSnapshot,
    lab: serde_json::Value,
) -> std::result::Result<String, String> {
    let provenance = verify_lab_workspace(
        expected_remote_component_path,
        materialized_workspace_path,
        snapshot,
        lab,
    )?;
    if provenance.materialization_mode == "git" {
        return Err(
            "does not require a synthetic Git baseline for git materialization".to_string(),
        );
    }
    if materialized_workspace_path.join(".git").exists() {
        // A replay reaches the same accepted snapshot after a prior worker has
        // already materialized its deterministic baseline. Reuse it only after
        // validating every provenance and Git-root invariant.
        verify_lab_workspace_git_root(materialized_workspace_path, &provenance)?;
        return git(materialized_workspace_path, &["rev-parse", "HEAD"]);
    }
    if let Some(path) = nested_git_metadata(materialized_workspace_path)? {
        return Err(format!(
            "snapshot workspace contains nested Git metadata at {}",
            path.display()
        ));
    }

    git(
        materialized_workspace_path,
        &[
            "init",
            "--quiet",
            "--initial-branch=homeboy-snapshot-baseline",
        ],
    )?;
    git(materialized_workspace_path, &["add", "--all"])?;
    let tree = git(materialized_workspace_path, &["write-tree"])?;
    let message = synthetic_snapshot_baseline_message(&provenance);
    let commit = git_with_env(
        materialized_workspace_path,
        &["commit-tree", &tree, "-m", &message],
        &[
            ("GIT_AUTHOR_NAME", "Homeboy Snapshot"),
            ("GIT_AUTHOR_EMAIL", "snapshot@homeboy.invalid"),
            ("GIT_COMMITTER_NAME", "Homeboy Snapshot"),
            ("GIT_COMMITTER_EMAIL", "snapshot@homeboy.invalid"),
            ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z"),
            ("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z"),
        ],
    )?;
    git(
        materialized_workspace_path,
        &["update-ref", SYNTHETIC_SNAPSHOT_BASELINE_REF, &commit],
    )?;
    Ok(commit)
}

/// Verifies the Git state accepted for a Lab materialization. Snapshot modes
/// may only use Homeboy's deterministic synthetic baseline.
pub(crate) fn verify_lab_workspace_git_root(
    workspace: &Path,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> std::result::Result<(), String> {
    match provenance.materialization_mode.as_str() {
        "git" => verify_git_materialization_root(workspace, provenance),
        "snapshot" => verify_synthetic_snapshot_git_baseline(workspace, provenance),
        "snapshot-git" => verify_materialized_snapshot_git_checkout(workspace, provenance),
        mode => Err(format!(
            "unsupported workspace materialization mode `{mode}`"
        )),
    }
}

fn verify_materialized_snapshot_git_checkout(
    workspace: &Path,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> std::result::Result<(), String> {
    verify_snapshot_workspace_content(workspace, provenance)?;
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("could not canonicalize workspace: {error}"))?;
    let git_root = PathBuf::from(git(workspace, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .map_err(|error| format!("could not canonicalize Git root: {error}"))?;
    if root != git_root {
        return Err("Git top-level does not exactly match the managed workspace root".to_string());
    }
    if let Some(path) = nested_git_metadata(workspace)? {
        return Err(format!(
            "snapshot workspace contains nested Git metadata at {}",
            path.display()
        ));
    }
    let expected_commit = provenance
        .synthetic_checkout_commit
        .as_deref()
        .filter(|value| is_git_revision(value))
        .ok_or("snapshot-git provenance is missing a valid synthetic checkout commit")?;
    let expected_ref = provenance
        .synthetic_checkout_ref
        .as_deref()
        .filter(|value| value.starts_with("refs/heads/"))
        .ok_or("snapshot-git provenance is missing a synthetic checkout branch ref")?;
    let expected_tree = provenance
        .synthetic_checkout_tree
        .as_deref()
        .filter(|value| is_git_revision(value))
        .ok_or("snapshot-git provenance is missing a valid synthetic checkout tree")?;
    if git(workspace, &["symbolic-ref", "--quiet", "HEAD"])? != expected_ref {
        return Err("snapshot-git HEAD is not on the recorded synthetic checkout ref".to_string());
    }
    if git(workspace, &["rev-parse", "HEAD"])? != expected_commit
        || git(workspace, &["rev-parse", expected_ref])? != expected_commit
    {
        return Err(
            "snapshot-git HEAD/ref does not match the recorded synthetic checkout commit"
                .to_string(),
        );
    }
    if !git(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("snapshot-git workspace is not clean".to_string());
    }
    if git(workspace, &["rev-parse", "HEAD^{tree}"])? != expected_tree
        || git(workspace, &["write-tree"])? != expected_tree
    {
        return Err("snapshot-git checkout tree does not match recorded provenance".to_string());
    }
    if git(workspace, &["rev-list", "--parents", "-n", "1", "HEAD"])?
        .split_whitespace()
        .count()
        != 1
    {
        return Err("snapshot-git synthetic checkout commit must not have parents".to_string());
    }
    let identity = provenance.workspace_identity.as_str();
    if git(workspace, &["log", "-1", "--format=%B"])? != format!("Homeboy snapshot {identity}") {
        return Err("snapshot-git commit message does not match its snapshot identity".to_string());
    }
    if git(
        workspace,
        &["show", "-s", "--format=%an <%ae>|%cn <%ce>", "HEAD"],
    )? != "Homeboy Snapshot <homeboy-snapshot@localhost>|Homeboy Snapshot <homeboy-snapshot@localhost>"
    {
        return Err("snapshot-git commit author/committer does not match Homeboy identity".to_string());
    }
    let expected_note = format!(
        "snapshot_identity={identity}\nsource_head={}",
        provenance.source_revision
    );
    if git(
        workspace,
        &["notes", "--ref=homeboy-snapshot", "show", "HEAD"],
    )? != expected_note
    {
        return Err("snapshot-git note does not match the verified source snapshot".to_string());
    }
    Ok(())
}

fn verify_git_materialization_root(
    workspace: &Path,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> std::result::Result<(), String> {
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("could not canonicalize workspace: {error}"))?;
    let git_root = git(workspace, &["rev-parse", "--show-toplevel"])?;
    let git_root = PathBuf::from(git_root)
        .canonicalize()
        .map_err(|error| format!("could not canonicalize Git root: {error}"))?;
    if root != git_root {
        return Err("Git top-level does not exactly match the managed workspace root".to_string());
    }
    let head = git(workspace, &["rev-parse", "HEAD"])?;
    if head != provenance.source_revision {
        return Err("Git HEAD does not match the verified source revision".to_string());
    }
    if !git(workspace, &["status", "--porcelain"])?.is_empty() {
        return Err("Git workspace is not clean".to_string());
    }
    Ok(())
}

fn verify_synthetic_snapshot_git_baseline(
    workspace: &Path,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> std::result::Result<(), String> {
    verify_snapshot_workspace_content(workspace, provenance)?;
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("could not canonicalize workspace: {error}"))?;
    let git_root = PathBuf::from(git(workspace, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .map_err(|error| format!("could not canonicalize Git root: {error}"))?;
    if root != git_root {
        return Err("Git top-level does not exactly match the managed workspace root".to_string());
    }
    if let Some(path) = nested_git_metadata(workspace)? {
        return Err(format!(
            "snapshot workspace contains nested Git metadata at {}",
            path.display()
        ));
    }
    if git(workspace, &["symbolic-ref", "--quiet", "HEAD"])? != SYNTHETIC_SNAPSHOT_BASELINE_REF {
        return Err("synthetic snapshot baseline HEAD is not on its deterministic ref".to_string());
    }
    let head = git(workspace, &["rev-parse", "HEAD"])?;
    if git(workspace, &["rev-parse", SYNTHETIC_SNAPSHOT_BASELINE_REF])? != head {
        return Err("synthetic snapshot baseline ref does not match HEAD".to_string());
    }
    if !git(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("synthetic snapshot Git workspace is not clean".to_string());
    }
    let tree = git(workspace, &["rev-parse", "HEAD^{tree}"])?;
    if git(workspace, &["write-tree"])? != tree {
        return Err(
            "synthetic snapshot baseline commit tree does not match the workspace".to_string(),
        );
    }
    let parents = git(workspace, &["rev-list", "--parents", "-n", "1", "HEAD"])?;
    if parents.split_whitespace().count() != 1 {
        return Err("synthetic snapshot baseline commit must not have parents".to_string());
    }
    let expected = synthetic_snapshot_baseline_commit(&tree, provenance);
    if git(workspace, &["cat-file", "commit", "HEAD"])? != expected {
        return Err(
            "synthetic snapshot baseline commit does not match verified provenance".to_string(),
        );
    }
    Ok(())
}

fn synthetic_snapshot_baseline_message(provenance: &VerifiedLabWorkspaceProvenance) -> String {
    format!(
        "homeboy snapshot baseline\n\nsource-revision: {}\nworkspace-identity: {}\nsnapshot-hash: {}",
        provenance.source_revision, provenance.workspace_identity, provenance.snapshot_hash,
    )
}

fn synthetic_snapshot_baseline_commit(
    tree: &str,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> String {
    format!(
        "tree {tree}\nauthor {SYNTHETIC_SNAPSHOT_AUTHOR}\ncommitter {SYNTHETIC_SNAPSHOT_AUTHOR}\n\n{}",
        synthetic_snapshot_baseline_message(provenance)
    )
}

/// Verifies a Lab-materialized workspace against its declared provenance.
/// Snapshot modes require byte-for-byte content parity; Git mode validates its
/// checkout identity separately because checkout normalization can change bytes.
pub(crate) fn verify_lab_workspace_from_env(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
) -> std::result::Result<VerifiedLabWorkspaceProvenance, String> {
    let snapshot: SourceSnapshot = env_json(SOURCE_SNAPSHOT_METADATA_ENV)
        .ok_or_else(|| "is missing source snapshot transport metadata".to_string())?;
    let lab: serde_json::Value = env_json(LAB_OFFLOAD_METADATA_ENV)
        .ok_or_else(|| "is missing Lab dispatch transport metadata".to_string())?;
    verify_lab_workspace(
        expected_remote_component_path,
        materialized_workspace_path,
        snapshot,
        lab,
    )
}

pub(crate) fn verify_lab_workspace(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
    snapshot: SourceSnapshot,
    lab: serde_json::Value,
) -> std::result::Result<VerifiedLabWorkspaceProvenance, String> {
    let recorded_remote_path = snapshot
        .remote_path
        .as_deref()
        .ok_or("is missing remote path")?;
    let source_revision = snapshot
        .git_sha
        .as_deref()
        .ok_or("is missing source revision")?;
    let workspace_identity = snapshot
        .workspace_snapshot_identity
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or("is missing workspace identity")?;
    let runner_id = lab
        .get("runner_id")
        .and_then(|value| value.as_str())
        .ok_or("is missing runner identity")?;
    let lab_remote_path = lab
        .get("remote_workspace")
        .and_then(|value| value.as_str())
        .ok_or("is missing remote workspace")?;
    let materialization_mode = lab
        .get("sync_mode")
        .and_then(|value| value.as_str())
        .ok_or("is missing materialization mode")?;
    let lab_snapshot = lab
        .get("source_snapshot")
        .ok_or("is missing source snapshot evidence")?;

    if snapshot.sync_mode != LAB_SOURCE_SNAPSHOT_SYNC_MODE {
        return Err(format!(
            "has untrusted source mode `{}`",
            snapshot.sync_mode
        ));
    }
    if !matches!(materialization_mode, "git" | "snapshot" | "snapshot-git") {
        return Err(format!(
            "has untrusted workspace materialization mode `{materialization_mode}`"
        ));
    }
    if materialization_mode == "git"
        && snapshot
            .sync_excludes
            .iter()
            .any(|exclude| exclude == ".git" || exclude == ".git/")
    {
        return Err("claims git materialization while excluding .git metadata".to_string());
    }
    if snapshot.dirty
        && lab
            .pointer("/workspace_cleanliness/allow_dirty_lab_workspace")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        return Err("records a dirty source checkout".to_string());
    }
    if !is_git_revision(source_revision) {
        return Err("has an invalid source revision".to_string());
    }
    if snapshot.runner_id.trim().is_empty() || snapshot.runner_id != runner_id {
        return Err("runner identity does not match the Lab dispatch".to_string());
    }
    if !paths_equal(
        expected_remote_component_path,
        &materialized_workspace_path.to_string_lossy(),
    ) || !paths_equal(
        recorded_remote_path,
        &materialized_workspace_path.to_string_lossy(),
    ) || !paths_equal(
        lab_remote_path,
        &materialized_workspace_path.to_string_lossy(),
    ) {
        return Err("remote workspace does not match materialized path".to_string());
    }
    if lab.get("status").and_then(|value| value.as_str()) != Some("offloaded") {
        return Err("dispatch status is not `offloaded`".to_string());
    }
    if serde_json::to_value(&snapshot).ok().as_ref() != Some(lab_snapshot) {
        return Err("source snapshot does not match Lab dispatch evidence".to_string());
    }
    let verification = lab.get("workspace_verification");
    let (expected_content_hash, verification_identity, content_hash_algorithm, permission_policy) =
        match verification {
            Some(verification) => {
                let schema = verification.get("schema").and_then(|value| value.as_str());
                let content_hash_algorithm = match schema {
                    Some("homeboy/lab-workspace-verification/v1") => {
                        "homeboy-workspace-content-v1".to_string()
                    }
                    Some("homeboy/lab-workspace-verification/v2") => {
                        let policy = verification
                            .get("permission_policy")
                            .and_then(|value| value.as_str())
                            .ok_or("is missing v2 workspace content permission policy")?;
                        let algorithm = workspace_content_hash_algorithm(policy).ok_or_else(|| {
                        format!("has an unsupported v2 workspace content permission policy `{policy}` on this platform")
                    })?;
                        if verification
                            .get("content_hash_algorithm")
                            .and_then(|value| value.as_str())
                            != Some(algorithm.as_str())
                        {
                            return Err("v2 workspace content hash algorithm does not bind its permission policy".to_string());
                        }
                        algorithm
                    }
                    Some(schema) => {
                        return Err(format!(
                            "has an unsupported workspace verification schema `{schema}`"
                        ))
                    }
                    None => return Err("is missing workspace verification schema".to_string()),
                };
                let identity = verification
                    .get("identity")
                    .and_then(|value| value.as_str())
                    .ok_or("is missing workspace verification identity")?;
                let content_hash = verification
                    .get("content_hash")
                    .and_then(|value| value.as_str())
                    .ok_or("is missing workspace verification content hash")?;
                let excludes = verification
                    .get("sync_excludes")
                    .ok_or("is missing workspace verification sync excludes")?;
                if excludes != &serde_json::json!(snapshot.sync_excludes) {
                    return Err("sync excludes do not match workspace verification".to_string());
                }
                if verification.get("source_snapshot") != Some(lab_snapshot) {
                    return Err("source snapshot does not match workspace verification".to_string());
                }
                let primary_workspace = verification
                    .get("primary_workspace")
                    .ok_or("is missing workspace verification primary workspace")?;
                if primary_workspace
                    .get("identity")
                    .and_then(|value| value.as_str())
                    != Some(identity)
                    || primary_workspace
                        .get("remote_path")
                        .and_then(|value| value.as_str())
                        != Some(recorded_remote_path)
                {
                    return Err(
                        "primary workspace does not match workspace verification".to_string()
                    );
                }
                (
                    content_hash,
                    identity,
                    content_hash_algorithm,
                    verification
                        .get("permission_policy")
                        .and_then(|value| value.as_str()),
                )
            }
            None if materialization_mode == "git" => {
                let content_hash = lab
                    .get("workspace_content_hash")
                    .and_then(|value| value.as_str())
                    .ok_or("is missing workspace content hash")?;
                let identity = lab
                    .get("workspace_materialization_plan")
                    .and_then(|value| value.get("identity"))
                    .and_then(|value| value.as_str())
                    .ok_or("is missing workspace materialization identity")?;
                (
                    content_hash,
                    identity,
                    "homeboy-workspace-content-v1".to_string(),
                    None,
                )
            }
            None => return Err("is missing workspace verification metadata".to_string()),
        };
    let content_manifest = verification
        .and_then(|verification| verification.get("content_manifest"))
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| format!("has invalid workspace content manifest: {error}"))?;
    if let Some(manifest) = &content_manifest {
        validate_content_manifest(manifest, permission_policy)?;
    }
    if workspace_identity != verification_identity {
        return Err("workspace identity does not match workspace verification".to_string());
    }
    let provenance = VerifiedLabWorkspaceProvenance {
        source_revision: source_revision.to_string(),
        materialization_mode: materialization_mode.to_string(),
        runner_id: snapshot.runner_id,
        workspace_identity: workspace_identity.to_string(),
        snapshot_hash: snapshot.snapshot_hash,
        content_hash: expected_content_hash.to_string(),
        content_hash_algorithm,
        permission_policy: permission_policy.map(str::to_string),
        content_manifest,
        sync_excludes: snapshot.sync_excludes,
        local_source_path: snapshot.local_path,
        remote_workspace_path: recorded_remote_path.to_string(),
        synthetic_checkout_commit: snapshot.synthetic_checkout_commit,
        synthetic_checkout_ref: snapshot.synthetic_checkout_ref,
        synthetic_checkout_tree: snapshot.synthetic_checkout_tree,
    };
    if provenance.materialization_mode != "git" {
        verify_snapshot_workspace_content(materialized_workspace_path, &provenance)?;
    }
    Ok(provenance)
}

fn verify_snapshot_workspace_content(
    workspace: &Path,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> std::result::Result<(), String> {
    let actual_content_hash = match provenance.content_hash_algorithm.as_str() {
        "homeboy-workspace-content-v1" => {
            workspace_content_hash_v1(workspace, &provenance.sync_excludes)
        }
        algorithm
            if algorithm.starts_with("homeboy-workspace-content-v2+")
                || algorithm == "homeboy-workspace-content-v3+unix-owner-executable" =>
        {
            workspace_content_hash_for_policy(
                workspace,
                &provenance.sync_excludes,
                provenance
                    .permission_policy
                    .as_deref()
                    .expect("v2 policy validated above"),
            )
        }
        _ => unreachable!("workspace verification algorithm was validated above"),
    }
    .map_err(|error| format!("could not hash materialized workspace: {}", error.message))?;
    if actual_content_hash != provenance.content_hash {
        let diagnostic = match provenance.materialization_mode.as_str() {
            "snapshot-git" => format!(
                "homeboy runner exec {} --cwd {} -- git status --short",
                shell::quote_arg(&provenance.runner_id),
                shell::quote_arg(&provenance.remote_workspace_path),
            ),
            "snapshot" => format!(
                "homeboy runner workspace sync --mode snapshot --path {} {}",
                shell::quote_arg(
                    provenance
                        .local_source_path
                        .as_deref()
                        .unwrap_or("<unknown-source-path>")
                ),
                shell::quote_arg(&provenance.runner_id),
            ),
            _ => unreachable!("workspace verification mode was validated above"),
        };
        let entry_diagnostic = provenance
            .content_manifest
            .as_ref()
            .and_then(|expected| {
                workspace_content_manifest_for_policy(
                    workspace,
                    &provenance.sync_excludes,
                    provenance.permission_policy.as_deref()?,
                )
                .ok()
                .map(|actual| content_manifest_difference(expected, &actual))
            })
            .unwrap_or_default();
        return Err(format!(
            "workspace content hash does not match the controller materialization using {} (expected {}, got {});{} operator diagnostic: `{diagnostic}`",
            provenance.content_hash_algorithm, provenance.content_hash, actual_content_hash, entry_diagnostic,
        ));
    }
    Ok(())
}

fn content_manifest_difference(
    expected: &WorkspaceContentManifest,
    actual: &WorkspaceContentManifest,
) -> String {
    let mut differing = Vec::with_capacity(CONTENT_MANIFEST_DIFFERENCE_LIMIT);
    for entry in &expected.entries {
        if differing.len() == CONTENT_MANIFEST_DIFFERENCE_LIMIT {
            break;
        }
        match actual
            .entries
            .iter()
            .find(|candidate| candidate.path == entry.path)
        {
            Some(candidate) if candidate == entry => {}
            Some(candidate) => differing.push(format!(
                "{} ({})",
                entry.path,
                content_manifest_entry_difference(entry, candidate)
            )),
            None => differing.push(format!("{} (missing)", entry.path)),
        }
    }
    for entry in &actual.entries {
        if differing.len() == CONTENT_MANIFEST_DIFFERENCE_LIMIT {
            break;
        }
        if !expected
            .entries
            .iter()
            .any(|candidate| candidate.path == entry.path)
        {
            differing.push(format!("{} (unexpected)", entry.path));
        }
    }
    format!(
        " entry diagnostics: {} differing logical entries in a bounded {}-entry sample (controller total {}, runner total {}): {};",
        differing.len(),
        expected.entries.len().max(actual.entries.len()),
        expected.entry_count,
        actual.entry_count,
        if differing.is_empty() { "sample metadata matched; content differs outside bounded metadata".to_string() } else { differing.join(", ") },
    )
}

fn content_manifest_entry_difference(
    expected: &WorkspaceContentManifestEntry,
    actual: &WorkspaceContentManifestEntry,
) -> &'static str {
    if expected.kind != actual.kind {
        "kind changed"
    } else if expected.owner_executable != actual.owner_executable {
        "owner-executable capability changed"
    } else {
        "metadata changed"
    }
}

fn validate_content_manifest(
    manifest: &WorkspaceContentManifest,
    permission_policy: Option<&str>,
) -> std::result::Result<(), String> {
    if manifest.entries.len() > 16 || manifest.entry_count < manifest.entries.len() {
        return Err("has invalid bounded workspace content manifest".to_string());
    }
    for entry in &manifest.entries {
        if entry.path.is_empty()
            || entry.path.len() > super::snapshot::WORKSPACE_CONTENT_DIAGNOSTIC_PATH_LIMIT
            || entry.path.contains('\0')
            || !matches!(entry.kind.as_str(), "file" | "directory")
        {
            return Err("has invalid bounded workspace content manifest entry".to_string());
        }
        if entry.kind == "directory" && entry.owner_executable.is_some() {
            return Err("has invalid directory workspace content manifest entry".to_string());
        }
        if entry.kind == "file"
            && permission_policy
                == Some(super::snapshot::WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE)
            && entry.owner_executable.is_none()
        {
            return Err("has incomplete v3 workspace content manifest entry".to_string());
        }
    }
    Ok(())
}

fn git(cwd: &Path, args: &[&str]) -> std::result::Result<String, String> {
    git_with_env(cwd, args, &[])
}

fn git_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> std::result::Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("could not run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn nested_git_metadata(workspace: &Path) -> std::result::Result<Option<PathBuf>, String> {
    fn visit(root: &Path, path: &Path) -> std::result::Result<Option<PathBuf>, String> {
        for entry in std::fs::read_dir(path)
            .map_err(|error| format!("could not inspect snapshot workspace: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("could not inspect snapshot entry: {error}"))?;
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") && path != root.join(".git") {
                return Ok(Some(path));
            }
            if entry
                .file_type()
                .map_err(|error| format!("could not inspect snapshot entry type: {error}"))?
                .is_dir()
            {
                if let Some(found) = visit(root, &path)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    visit(workspace, workspace)
}

fn env_json<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    std::env::var(name)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn paths_equal(left: &str, right: &str) -> bool {
    matches!((Path::new(left).canonicalize(), Path::new(right).canonicalize()), (Ok(left), Ok(right)) if left == right)
}

fn is_git_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::super::snapshot::{
        WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY, WORKSPACE_CONTENT_PERMISSION_PORTABLE,
    };
    use super::*;

    fn snapshot(path: &Path) -> SourceSnapshot {
        SourceSnapshot {
            runner_id: "lab".to_string(),
            local_path: Some("/controller/source".to_string()),
            remote_path: Some(path.display().to_string()),
            workspace_root: None,
            git_branch: Some("main".to_string()),
            git_sha: Some("a".repeat(40)),
            dirty: false,
            sync_mode: LAB_SOURCE_SNAPSHOT_SYNC_MODE.to_string(),
            workspace_snapshot_identity: Some("snapshot:verified-content".to_string()),
            synthetic_checkout_commit: None,
            synthetic_checkout_ref: None,
            synthetic_checkout_tree: None,
            snapshot_hash: "sha256:verified-source".to_string(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            sync_excludes: vec![".git".to_string(), ".git/**".to_string()],
        }
    }

    fn lab(path: &Path, snapshot: &SourceSnapshot) -> serde_json::Value {
        let content_hash = workspace_content_hash_v1(path, &snapshot.sync_excludes)
            .expect("legacy snapshot content hash");
        serde_json::json!({
            "runner_id": "lab",
            "remote_workspace": path.display().to_string(),
            "sync_mode": "snapshot",
            "status": "offloaded",
            "source_snapshot": snapshot,
            "workspace_verification": {
                "schema": "homeboy/lab-workspace-verification/v1",
                "identity": "snapshot:verified-content",
                "content_hash": content_hash,
                "sync_excludes": snapshot.sync_excludes,
                "source_snapshot": snapshot,
                "primary_workspace": {
                    "identity": "snapshot:verified-content",
                    "remote_path": path.display().to_string(),
                }
            }
        })
    }

    fn git_workspace() -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        git(workspace.path(), &["init", "--quiet"]).expect("initialize repository");
        git(workspace.path(), &["add", "--all"]).expect("stage source");
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        )
        .expect("commit source");
        workspace
    }

    fn git_snapshot(path: &Path) -> SourceSnapshot {
        let mut snapshot = snapshot(path);
        snapshot.git_sha = Some(git(path, &["rev-parse", "HEAD"]).expect("source revision"));
        snapshot.sync_excludes = Vec::new();
        snapshot
    }

    fn git_lab(path: &Path, snapshot: &SourceSnapshot) -> serde_json::Value {
        let mut lab = lab(path, snapshot);
        lab["sync_mode"] = serde_json::json!("git");
        lab["workspace_verification"]["content_hash"] = serde_json::json!("controller-byte-hash");
        lab
    }

    fn materialized_snapshot_workspace() -> (tempfile::TempDir, VerifiedLabWorkspaceProvenance) {
        materialized_snapshot_workspace_for_mode("snapshot")
    }

    fn materialized_snapshot_workspace_for_mode(
        mode: &str,
    ) -> (tempfile::TempDir, VerifiedLabWorkspaceProvenance) {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let mut lab = lab(workspace.path(), &snapshot);
        lab["sync_mode"] = serde_json::json!(mode);
        let provenance = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab.clone(),
        )
        .expect("verified snapshot provenance");
        materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            lab,
        )
        .expect("synthetic baseline");
        (workspace, provenance)
    }

    fn materialized_production_snapshot_git_workspace(
    ) -> (tempfile::TempDir, VerifiedLabWorkspaceProvenance) {
        let source = git_workspace();
        let remote_root = tempfile::tempdir().expect("runner root");
        let remote = remote_root.path().join("workspace");
        let excludes = vec![".git".to_string(), ".git/**".to_string()];
        let identity = super::super::snapshot::snapshot_identity(source.path(), &excludes, &[])
            .expect("snapshot identity");
        let runner: crate::Runner = serde_json::from_value(serde_json::json!({
            "id": "lab", "kind": "local"
        }))
        .expect("local runner");
        let synthetic = super::super::snapshot::materialize_snapshot_git(
            &runner,
            source.path(),
            &remote.display().to_string(),
            &excludes,
            &identity,
        )
        .expect("production snapshot-git materialization");
        let mut snapshot = SourceSnapshot::collect_local(
            "lab",
            source.path(),
            Some(&remote.display().to_string()),
            LAB_SOURCE_SNAPSHOT_SYNC_MODE,
        );
        snapshot.sync_excludes = excludes;
        snapshot.workspace_snapshot_identity = Some(identity);
        snapshot.synthetic_checkout_commit = Some(synthetic.synthetic_commit);
        snapshot.synthetic_checkout_ref = Some(synthetic.synthetic_ref);
        snapshot.synthetic_checkout_tree = Some(synthetic.synthetic_tree);
        let mut lab = lab(&remote, &snapshot);
        lab["sync_mode"] = serde_json::json!("snapshot-git");
        lab["workspace_verification"]["identity"] =
            serde_json::json!(snapshot.workspace_snapshot_identity);
        lab["workspace_verification"]["primary_workspace"]["identity"] =
            serde_json::json!(snapshot.workspace_snapshot_identity);
        let provenance =
            verify_lab_workspace(&remote.display().to_string(), &remote, snapshot, lab)
                .expect("verified production snapshot-git provenance");
        (remote_root, provenance)
    }

    #[test]
    fn git_materialization_accepts_checkout_normalization_hash_difference() {
        let workspace = git_workspace();
        let snapshot = git_snapshot(workspace.path());
        let provenance = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            git_lab(workspace.path(), &snapshot),
        )
        .expect("Git provenance accepts non-authoritative byte hash");

        verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect("clean checkout at expected revision");
    }

    #[test]
    fn git_materialization_rejects_wrong_head_root_identity_and_dirty_workspace() {
        let workspace = git_workspace();
        let snapshot = git_snapshot(workspace.path());
        let lab = git_lab(workspace.path(), &snapshot);
        let provenance = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab.clone(),
        )
        .expect("initial Git provenance");

        std::fs::write(workspace.path().join("file.txt"), "changed\n").expect("change source");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("dirty Git workspace must fail closed")
            .contains("not clean"));
        git(workspace.path(), &["checkout", "--", "file.txt"]).expect("restore source");
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "wrong head",
            ],
        )
        .expect("advance head");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("wrong Git HEAD must fail closed")
            .contains("HEAD does not match"));

        let nested_root = workspace.path().join("nested");
        std::fs::create_dir(&nested_root).expect("nested path");
        assert!(verify_lab_workspace_git_root(&nested_root, &provenance)
            .expect_err("wrong managed root must fail closed")
            .contains("top-level does not exactly match"));

        let mut wrong_identity = lab;
        wrong_identity["workspace_verification"]["identity"] = serde_json::json!("other");
        wrong_identity["workspace_verification"]["primary_workspace"]["identity"] =
            serde_json::json!("other");
        assert!(verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            wrong_identity,
        )
        .expect_err("wrong declared identity must fail closed")
        .contains("workspace identity"));
    }

    #[test]
    fn snapshot_materializations_reject_content_hash_mismatch() {
        for mode in ["snapshot", "snapshot-git"] {
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
            let snapshot = snapshot(workspace.path());
            let mut lab = lab(workspace.path(), &snapshot);
            lab["sync_mode"] = serde_json::json!(mode);
            lab["workspace_verification"]["content_hash"] = serde_json::json!("wrong-hash");

            assert!(verify_lab_workspace(
                &workspace.path().display().to_string(),
                workspace.path(),
                snapshot,
                lab,
            )
            .expect_err("snapshot hash mismatch must fail closed")
            .contains("content hash"));
        }
    }

    #[test]
    fn content_hash_mismatch_reports_bounded_logical_entry_diagnostics() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file-00.txt"), "controller secret\n")
            .expect("source file");
        for index in 1..20 {
            std::fs::write(
                workspace.path().join(format!("file-{index:02}.txt")),
                "baseline\n",
            )
            .expect("source file");
        }
        let snapshot = snapshot(workspace.path());
        let content_hash =
            super::super::workspace_content_hash(workspace.path(), &snapshot.sync_excludes)
                .expect("content hash");
        let content_manifest = workspace_content_manifest_for_policy(
            workspace.path(),
            &snapshot.sync_excludes,
            WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
        )
        .expect("content manifest");
        let mut v2 = lab(workspace.path(), &snapshot);
        v2["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v2");
        v2["workspace_verification"]["permission_policy"] =
            serde_json::json!(WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY);
        v2["workspace_verification"]["content_hash_algorithm"] = serde_json::json!(
            workspace_content_hash_algorithm(WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY)
                .expect("content hash algorithm")
        );
        v2["workspace_verification"]["content_hash"] = serde_json::json!(content_hash);
        v2["workspace_verification"]["content_manifest"] =
            serde_json::to_value(content_manifest).expect("manifest JSON");

        std::fs::write(workspace.path().join("file-00.txt"), "runner mutation\n")
            .expect("mutate materialization");
        let error = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            v2,
        )
        .expect_err("changed materialization must fail");

        assert!(error.contains("entry diagnostics: 0 differing logical entries"));
        assert!(error.contains("sample metadata matched; content differs outside bounded metadata"));
        assert!(error.contains("bounded 16-entry sample"));
        assert!(!error.contains("controller secret"));
        assert!(!error.contains("runner mutation"));
    }

    #[test]
    fn content_manifest_validation_rejects_oversized_and_long_path_metadata() {
        let entry = WorkspaceContentManifestEntry {
            path: "file.txt".to_string(),
            kind: "file".to_string(),
            owner_executable: Some(false),
        };
        let oversized = WorkspaceContentManifest {
            entry_count: 17,
            entries: vec![entry.clone(); 17],
        };
        assert!(validate_content_manifest(&oversized, None).is_err());

        let long_path = WorkspaceContentManifest {
            entry_count: 1,
            entries: vec![WorkspaceContentManifestEntry {
                path: "a"
                    .repeat(super::super::snapshot::WORKSPACE_CONTENT_DIAGNOSTIC_PATH_LIMIT + 1),
                ..entry
            }],
        };
        assert!(validate_content_manifest(&long_path, None).is_err());
    }

    #[test]
    fn verified_snapshot_baseline_supports_committed_and_uncommitted_candidate_harvesting() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let baseline = materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab(workspace.path(), &snapshot),
        )
        .expect("verified snapshot baseline");

        assert_eq!(
            git(workspace.path(), &["rev-parse", "HEAD"]).unwrap(),
            baseline
        );
        let message = git(workspace.path(), &["log", "-1", "--format=%B"]).unwrap();
        assert!(message.contains("source-revision: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(message.contains("workspace-identity: snapshot:verified-content"));
        assert!(message.contains("snapshot-hash: sha256:verified-source"));
        std::fs::write(workspace.path().join("file.txt"), "candidate\n").expect("candidate");
        git(workspace.path(), &["add", "--all"]).expect("stage candidate");
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "-m",
                "provider candidate",
            ],
        )
        .expect("commit candidate");
        let committed_patch = git(workspace.path(), &["diff", "--binary", &baseline, "HEAD"])
            .expect("committed candidate patch");
        assert!(committed_patch.contains("-baseline"));
        assert!(committed_patch.contains("+candidate"));

        std::fs::write(workspace.path().join("file.txt"), "uncommitted candidate\n")
            .expect("uncommitted candidate");
        git(workspace.path(), &["add", "--all"]).expect("stage uncommitted candidate");
        let uncommitted_patch = git(workspace.path(), &["diff", "--cached", "--binary", "HEAD"])
            .expect("candidate patch");
        assert!(uncommitted_patch.contains("-candidate"));
        assert!(uncommitted_patch.contains("+uncommitted candidate"));
    }

    #[test]
    fn verified_snapshot_baseline_replay_reuses_only_the_matching_baseline() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let lab = lab(workspace.path(), &snapshot);
        let baseline = materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab.clone(),
        )
        .expect("initial snapshot baseline");

        assert_eq!(
            materialize_verified_lab_snapshot_git_baseline(
                &workspace.path().display().to_string(),
                workspace.path(),
                snapshot.clone(),
                lab.clone(),
            )
            .expect("replayed snapshot baseline"),
            baseline,
        );

        let mut mismatched_snapshot = snapshot;
        mismatched_snapshot.git_sha = Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
        let error = materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            mismatched_snapshot,
            lab,
        )
        .expect_err("mismatched accepted source provenance must fail");
        assert!(!error.is_empty());
    }

    #[test]
    fn synthetic_snapshot_baseline_rejects_foreign_and_tampered_git_state() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let provenance = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab(workspace.path(), &snapshot),
        )
        .expect("verified snapshot provenance");
        git(workspace.path(), &["init", "--quiet"]).expect("foreign Git root");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance).is_err());

        let (workspace, provenance) = materialized_snapshot_workspace();
        verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect("deterministic synthetic baseline");
        git(workspace.path(), &["checkout", "--quiet", "--detach"])
            .expect("detach synthetic baseline");
        verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("changed baseline HEAD must fail");

        let (workspace, provenance) = materialized_snapshot_workspace();
        let tree = git(workspace.path(), &["rev-parse", "HEAD^{tree}"]).expect("baseline tree");
        let changed = git_with_env(
            workspace.path(),
            &["commit-tree", &tree, "-m", "changed baseline message"],
            &[
                ("GIT_AUTHOR_NAME", "Homeboy Snapshot"),
                ("GIT_AUTHOR_EMAIL", "snapshot@homeboy.invalid"),
                ("GIT_COMMITTER_NAME", "Homeboy Snapshot"),
                ("GIT_COMMITTER_EMAIL", "snapshot@homeboy.invalid"),
                ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z"),
                ("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z"),
            ],
        )
        .expect("changed message commit");
        git(
            workspace.path(),
            &["update-ref", SYNTHETIC_SNAPSHOT_BASELINE_REF, &changed],
        )
        .expect("move baseline ref");
        git(workspace.path(), &["reset", "--hard", "--quiet"])
            .expect("checkout changed baseline tree");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("changed baseline message must fail")
            .contains("does not match verified provenance"));

        let (workspace, provenance) = materialized_snapshot_workspace();
        std::fs::write(workspace.path().join("file.txt"), "other baseline\n")
            .expect("change source tree");
        git(workspace.path(), &["add", "--all"]).expect("stage changed tree");
        let tree = git(workspace.path(), &["write-tree"]).expect("changed tree");
        let changed = git_with_env(
            workspace.path(),
            &[
                "commit-tree",
                &tree,
                "-m",
                &synthetic_snapshot_baseline_message(&provenance),
            ],
            &[
                ("GIT_AUTHOR_NAME", "Homeboy Snapshot"),
                ("GIT_AUTHOR_EMAIL", "snapshot@homeboy.invalid"),
                ("GIT_COMMITTER_NAME", "Homeboy Snapshot"),
                ("GIT_COMMITTER_EMAIL", "snapshot@homeboy.invalid"),
                ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z"),
                ("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z"),
            ],
        )
        .expect("changed tree commit");
        git(
            workspace.path(),
            &["update-ref", SYNTHETIC_SNAPSHOT_BASELINE_REF, &changed],
        )
        .expect("move baseline ref");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("changed baseline tree must fail")
            .contains("content hash"));

        let (workspace, provenance) = materialized_snapshot_workspace();
        std::fs::write(workspace.path().join("file.txt"), "changed\n").expect("change file");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("dirty synthetic workspace must fail")
            .contains("content hash"));

        let (workspace, mut provenance) = materialized_snapshot_workspace();
        provenance.snapshot_hash = "sha256:other-source".to_string();
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("mismatched snapshot metadata must fail")
            .contains("does not match verified provenance"));
    }

    #[test]
    fn production_snapshot_git_checkout_verifier_rejects_tampering() {
        let (workspace, provenance) = materialized_production_snapshot_git_workspace();
        verify_lab_workspace_git_root(workspace.path().join("workspace").as_path(), &provenance)
            .expect("production snapshot-git checkout");

        let workspace_path = workspace.path().join("workspace");
        git(&workspace_path, &["checkout", "--quiet", "--detach"])
            .expect("detach snapshot-git checkout");
        verify_lab_workspace_git_root(&workspace_path, &provenance)
            .expect_err("snapshot-git ref/HEAD tampering must fail");

        let (workspace, provenance) = materialized_production_snapshot_git_workspace();
        let workspace_path = workspace.path().join("workspace");
        let tree = git(&workspace_path, &["rev-parse", "HEAD^{tree}"]).expect("baseline tree");
        let changed = git(
            &workspace_path,
            &["commit-tree", &tree, "-m", "changed message"],
        )
        .expect("changed message commit");
        let reference = provenance.synthetic_checkout_ref.as_deref().unwrap();
        git(&workspace_path, &["update-ref", reference, &changed]).expect("move ref");
        verify_lab_workspace_git_root(&workspace_path, &provenance)
            .expect_err("snapshot-git message/commit tampering must fail");

        let (workspace, provenance) = materialized_production_snapshot_git_workspace();
        let workspace_path = workspace.path().join("workspace");
        std::fs::write(workspace_path.join("file.txt"), "changed tree\n").expect("change tree");
        git(&workspace_path, &["add", "--all"]).expect("stage changed tree");
        let tree = git(&workspace_path, &["write-tree"]).expect("changed tree");
        let changed = git(
            &workspace_path,
            &["commit-tree", &tree, "-m", "changed tree"],
        )
        .expect("changed tree commit");
        let reference = provenance.synthetic_checkout_ref.as_deref().unwrap();
        git(&workspace_path, &["update-ref", reference, &changed]).expect("move ref");
        git(&workspace_path, &["reset", "--hard", "--quiet"]).expect("checkout changed tree");
        verify_lab_workspace_git_root(&workspace_path, &provenance)
            .expect_err("snapshot-git tree tampering must fail");
    }

    #[test]
    fn snapshot_baseline_rejects_invalid_provenance_before_creating_git_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let mut snapshot = snapshot(workspace.path());
        snapshot.git_sha = Some("invalid".to_string());

        let error = materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab(workspace.path(), &snapshot),
        )
        .expect_err("invalid provenance must fail closed");

        assert!(error.contains("invalid source revision"));
        assert!(!workspace.path().join(".git").exists());
    }

    #[test]
    #[cfg(unix)]
    fn verifier_accepts_legacy_v1_v2_and_current_v3_content_hashes() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());

        verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab(workspace.path(), &snapshot),
        )
        .expect("legacy v1 metadata remains valid");

        let content_hash = workspace_content_hash_for_policy(
            workspace.path(),
            &snapshot.sync_excludes,
            super::super::snapshot::WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
        )
        .expect("historical v2 content hash");
        let mut v2 = lab(workspace.path(), &snapshot);
        v2["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v2");
        v2["workspace_verification"]["permission_policy"] =
            serde_json::json!(super::super::snapshot::WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE);
        v2["workspace_verification"]["content_hash_algorithm"] =
            serde_json::json!(workspace_content_hash_algorithm(
                super::super::snapshot::WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE
            )
            .expect("historical v2 content hash algorithm"));
        v2["workspace_verification"]["content_hash"] = serde_json::json!(content_hash);
        verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            v2,
        )
        .expect("historical v2 metadata remains valid");

        let content_hash =
            super::super::workspace_content_hash(workspace.path(), &snapshot.sync_excludes)
                .expect("current v3 content hash");
        let mut v3 = lab(workspace.path(), &snapshot);
        v3["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v2");
        v3["workspace_verification"]["permission_policy"] =
            serde_json::json!(WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY);
        v3["workspace_verification"]["content_hash_algorithm"] = serde_json::json!(
            workspace_content_hash_algorithm(WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY)
                .expect("current v3 content hash algorithm")
        );
        v3["workspace_verification"]["content_hash"] = serde_json::json!(content_hash);
        verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            v3,
        )
        .expect("current v3 metadata verifies");
    }

    #[test]
    fn verifier_accepts_portable_v2_content_hash_on_all_platforms() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let content_hash = workspace_content_hash_for_policy(
            workspace.path(),
            &snapshot.sync_excludes,
            WORKSPACE_CONTENT_PERMISSION_PORTABLE,
        )
        .expect("portable v2 content hash");
        let mut v2 = lab(workspace.path(), &snapshot);
        v2["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v2");
        v2["workspace_verification"]["permission_policy"] =
            serde_json::json!(WORKSPACE_CONTENT_PERMISSION_PORTABLE);
        v2["workspace_verification"]["content_hash_algorithm"] = serde_json::json!(
            workspace_content_hash_algorithm(WORKSPACE_CONTENT_PERMISSION_PORTABLE)
                .expect("portable v2 content hash algorithm")
        );
        v2["workspace_verification"]["content_hash"] = serde_json::json!(content_hash);
        verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            v2,
        )
        .expect("portable v2 metadata remains valid");
    }

    #[test]
    fn verifier_rejects_unknown_or_incomplete_verification_versions_clearly() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let mut newer = lab(workspace.path(), &snapshot);
        newer["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v3");
        let error = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            newer,
        )
        .expect_err("older verifier must reject newer schema");
        assert!(error.contains(
            "unsupported workspace verification schema `homeboy/lab-workspace-verification/v3`"
        ));

        let mut incomplete = lab(workspace.path(), &snapshot);
        incomplete["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v2");
        let error = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            incomplete,
        )
        .expect_err("v2 requires explicit algorithm");
        assert!(error.contains("missing v2 workspace content permission policy"));

        let mut policy_mismatch = lab(workspace.path(), &snapshot);
        policy_mismatch["workspace_verification"]["schema"] =
            serde_json::json!("homeboy/lab-workspace-verification/v2");
        policy_mismatch["workspace_verification"]["permission_policy"] =
            serde_json::json!(WORKSPACE_CONTENT_PERMISSION_PORTABLE);
        policy_mismatch["workspace_verification"]["content_hash_algorithm"] =
            serde_json::json!("homeboy-workspace-content-v2+unix-executable");
        let error = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            policy_mismatch,
        )
        .expect_err("v2 algorithm must bind the declared policy");
        assert!(error.contains("does not bind its permission policy"));
    }

    #[test]
    fn mismatch_diagnostic_uses_git_status_only_for_git_materializations() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let mut git_materialized = lab(workspace.path(), &snapshot);
        let snapshot_materialized = lab(workspace.path(), &snapshot);
        git_materialized["sync_mode"] = serde_json::json!("snapshot-git");
        std::fs::write(workspace.path().join("file.txt"), "changed\n").expect("change file");
        let error = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            git_materialized,
        )
        .expect_err("changed snapshot-git workspace must fail");
        assert!(error.contains("runner exec lab"));
        assert!(error.contains("git status --short"));

        let error = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            snapshot_materialized,
        )
        .expect_err("changed snapshot workspace must fail");
        assert!(error.contains("runner workspace sync --mode snapshot"));
        assert!(!error.contains("git status --short"));
    }
}
