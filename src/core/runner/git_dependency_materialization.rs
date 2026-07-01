use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::rig::spec::DependencyCacheSpec;

use super::{
    workspace::{
        canonical_workspace_path, effective_snapshot_excludes, git_output, local_snapshot_stats,
        materialize_snapshot, materialize_snapshot_git, parent_remote_path, run_shell_capture,
        run_shell_command, shell_command_for_runner, snapshot_identity, ByteFileCounts,
        DEFAULT_EXCLUDES,
    },
    Runner, RunnerKind, RunnerWorkspaceSyncMode,
};

pub(crate) const DEPENDENCY_CACHE_SCHEMA: &str = "homeboy/runner-dependency-cache/v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RunnerGitDependencyMaterializationOutput {
    pub local_path: String,
    pub remote_path: String,
    pub remote_url: String,
    pub head: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_subpath: Option<String>,
    pub used_pinned_ref: bool,
    /// True when the snapshot includes dirty tracked and/or untracked working
    /// tree changes (explicit `--allow-dirty-lab-workspace` overlay) rather than
    /// a clean checkout at HEAD. Makes bench artifact provenance explicit.
    pub dirty_overlay: bool,
    pub sync_mode: RunnerWorkspaceSyncMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_cache: Option<RunnerDependencyCacheRestoreOutput>,
    #[serde(flatten)]
    pub counts: ByteFileCounts,
}

#[derive(Debug, Clone)]
pub(crate) struct RunnerGitDependencyMaterializationOptions {
    pub local_path: String,
    pub remote_path: String,
    pub remote_url: Option<String>,
    pub required_subpath: Option<String>,
    pub pinned_ref: Option<String>,
    /// When true, a dirty/untracked working tree is snapshotted as-is (overlay)
    /// instead of being refused. The snapshot already tars the working tree, so
    /// the dirty overlay travels to the runner. Defaults to false (clean-HEAD).
    pub allow_dirty: bool,
    pub dependency_cache: Option<DependencyCacheSpec>,
    pub component_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RunnerDependencyCacheRestoreOutput {
    pub schema: &'static str,
    pub status: String,
    pub key: String,
    pub cache_path: String,
    pub restored_paths: Vec<String>,
    pub missing_paths: Vec<String>,
    pub manifest: RunnerDependencyCacheManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RunnerDependencyCacheSaveOutput {
    pub schema: &'static str,
    pub status: String,
    pub key: String,
    pub cache_path: String,
    pub saved_paths: Vec<String>,
    pub missing_paths: Vec<String>,
    pub manifest: RunnerDependencyCacheManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RunnerDependencyCacheManifest {
    pub schema: &'static str,
    pub key: String,
    pub step_id: String,
    pub component_ref: String,
    pub runner_os: String,
    pub runner_arch: String,
    pub key_files: Vec<RunnerDependencyCacheKeyFile>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RunnerDependencyCacheKeyFile {
    pub role: String,
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RunnerDependencyCacheSaveRequest {
    pub remote_path: String,
    pub cache_path: String,
    pub manifest: RunnerDependencyCacheManifest,
}

pub(crate) fn materialize_git_dependency(
    runner: &Runner,
    options: RunnerGitDependencyMaterializationOptions,
) -> Result<RunnerGitDependencyMaterializationOutput> {
    let local_path = canonical_workspace_path(&options.local_path)?;
    if let Some(subpath) = options
        .required_subpath
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let required_path = local_path.join(subpath);
        if !required_path.is_dir() {
            return Err(Error::validation_invalid_argument(
                "rig_component_dependency",
                "rig dependency snapshot is missing required subpath",
                Some(required_path.display().to_string()),
                None,
            ));
        }
    }

    let remote_url = match options.remote_url {
        Some(remote_url) if !remote_url.trim().is_empty() => remote_url,
        _ => git_output(&local_path, &["config", "--get", "remote.origin.url"]).unwrap_or_default(),
    };
    let freshness = ensure_git_dependency_fresh(
        &local_path,
        options.pinned_ref.as_deref(),
        options.allow_dirty,
    )?;
    let dirty_overlay = freshness.status == DependencyUpdateStatus::DirtyOverlayAllowed;
    let mut excludes = DEFAULT_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for pattern in &runner.policy.snapshot_excludes {
        if !excludes.contains(pattern) {
            excludes.push(pattern.clone());
        }
    }
    let excludes = effective_snapshot_excludes(excludes, &runner.policy.snapshot_includes);
    let snapshot = snapshot_identity(&local_path, &excludes, &runner.policy.snapshot_includes)?;
    let stats = local_snapshot_stats(&local_path, &excludes, &runner.policy.snapshot_includes)?;
    // The default snapshot excludes strip `.git`/`.git/**`, so a plain
    // `materialize_snapshot` lands a runner-side component path with NO git
    // provenance (no HEAD, no refs). Canonical trace preflight probes the
    // materialized component path for git provenance and rejects it as
    // `not-git` before the workload starts (#4314). When the source checkout is
    // a real git worktree, seed a synthetic git checkout on the runner so the
    // materialized path carries canonical provenance (a committed HEAD at the
    // snapshot identity, with the source commit recorded), letting trace
    // preflight accept it. A non-git source has no provenance to preserve, so it
    // keeps the plain snapshot.
    if freshness.status == DependencyUpdateStatus::NotGit {
        materialize_snapshot(runner, &local_path, &options.remote_path, &excludes)?;
    } else {
        materialize_snapshot_git(
            runner,
            &local_path,
            &options.remote_path,
            &excludes,
            &snapshot,
        )?;
    }
    let dependency_cache = options
        .dependency_cache
        .as_ref()
        .map(|cache| {
            dependency_cache_restore(
                runner,
                &local_path,
                &options.remote_path,
                cache,
                options
                    .component_ref
                    .as_deref()
                    .or(freshness.after_sha.as_deref())
                    .or(freshness.pinned_ref.as_deref())
                    .unwrap_or(&snapshot),
            )
        })
        .transpose()?;

    Ok(RunnerGitDependencyMaterializationOutput {
        local_path: local_path.display().to_string(),
        remote_path: options.remote_path,
        remote_url,
        head: snapshot,
        status: freshness.status.label().to_string(),
        branch: freshness.branch,
        before_sha: freshness.before_sha,
        after_sha: freshness.after_sha,
        upstream_sha: freshness.upstream_sha,
        upstream: freshness.upstream,
        pinned_ref: freshness.pinned_ref,
        required_subpath: options.required_subpath,
        used_pinned_ref: freshness.used_pinned_ref,
        dirty_overlay,
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        dependency_cache,
        counts: stats,
    })
}

pub(crate) fn dependency_cache_save(
    runner: &Runner,
    request: &RunnerDependencyCacheSaveRequest,
) -> Result<RunnerDependencyCacheSaveOutput> {
    save_dependency_cache_paths(
        runner,
        &request.remote_path,
        &request.cache_path,
        &request.manifest,
    )
}

pub(crate) fn dependency_cache_save_request(
    output: &RunnerGitDependencyMaterializationOutput,
) -> Option<RunnerDependencyCacheSaveRequest> {
    let cache = output.dependency_cache.as_ref()?;
    Some(RunnerDependencyCacheSaveRequest {
        remote_path: output.remote_path.clone(),
        cache_path: cache.cache_path.clone(),
        manifest: cache.manifest.clone(),
    })
}

fn dependency_cache_restore(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    spec: &DependencyCacheSpec,
    component_ref: &str,
) -> Result<RunnerDependencyCacheRestoreOutput> {
    let manifest = dependency_cache_manifest(runner, local_path, spec, component_ref)?;
    let cache_path = dependency_cache_path(runner, remote_path, &manifest.key);
    restore_dependency_cache_paths(runner, remote_path, &cache_path, &manifest)
}

fn dependency_cache_manifest(
    runner: &Runner,
    local_path: &Path,
    spec: &DependencyCacheSpec,
    component_ref: &str,
) -> Result<RunnerDependencyCacheManifest> {
    validate_dependency_cache_spec(spec)?;
    let runner_os = runner_label(runner, "os", std::env::consts::OS);
    let runner_arch = runner_label(runner, "arch", std::env::consts::ARCH);
    let mut key_files = Vec::new();
    for path in &spec.lockfiles {
        key_files.push(cache_key_file(local_path, "lockfile", path)?);
    }
    for path in &spec.package_metadata {
        key_files.push(cache_key_file(local_path, "package_metadata", path)?);
    }
    key_files.sort_by(|left, right| {
        left.role
            .cmp(&right.role)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut paths = spec
        .paths
        .iter()
        .map(|path| normalize_relative_cache_path(path))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    paths.dedup();
    let mut hasher = Sha256::new();
    hasher.update(DEPENDENCY_CACHE_SCHEMA.as_bytes());
    hasher.update(spec.step_id.trim().as_bytes());
    hasher.update(component_ref.as_bytes());
    hasher.update(runner_os.as_bytes());
    hasher.update(runner_arch.as_bytes());
    for file in &key_files {
        hasher.update(file.role.as_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(file.status.as_bytes());
        if let Some(sha) = &file.sha256 {
            hasher.update(sha.as_bytes());
        }
    }
    for path in &paths {
        hasher.update(path.as_bytes());
    }
    let key = format!("dep-cache-{}", hex(&hasher.finalize()));
    Ok(RunnerDependencyCacheManifest {
        schema: DEPENDENCY_CACHE_SCHEMA,
        key,
        step_id: spec.step_id.trim().to_string(),
        component_ref: component_ref.to_string(),
        runner_os,
        runner_arch,
        key_files,
        paths,
    })
}

fn restore_dependency_cache_paths(
    runner: &Runner,
    remote_path: &str,
    cache_path: &str,
    manifest: &RunnerDependencyCacheManifest,
) -> Result<RunnerDependencyCacheRestoreOutput> {
    let mut restored = Vec::new();
    let mut missing = Vec::new();
    for path in &manifest.paths {
        let archive = cache_archive_path(cache_path, path);
        let probe = format!(
            "if test -f {}; then printf yes; fi",
            shell::quote_arg(&archive)
        );
        if run_shell_capture(&shell_command_for_runner(runner, &probe)?).is_none() {
            missing.push(path.clone());
            continue;
        }
        let command = format!(
            "mkdir -p {dest} && rm -rf {target} && tar -C {dest} -xf {archive}",
            dest = shell::quote_arg(remote_path),
            target = shell::quote_arg(&Path::new(remote_path).join(path).display().to_string()),
            archive = shell::quote_arg(&archive),
        );
        run_shell_command(
            &shell_command_for_runner(runner, &command)?,
            "restore runner dependency cache",
        )?;
        restored.push(path.clone());
    }
    Ok(RunnerDependencyCacheRestoreOutput {
        schema: DEPENDENCY_CACHE_SCHEMA,
        status: if restored.is_empty() { "miss" } else { "hit" }.to_string(),
        key: manifest.key.clone(),
        cache_path: cache_path.to_string(),
        restored_paths: restored,
        missing_paths: missing,
        manifest: manifest.clone(),
    })
}

fn save_dependency_cache_paths(
    runner: &Runner,
    remote_path: &str,
    cache_path: &str,
    manifest: &RunnerDependencyCacheManifest,
) -> Result<RunnerDependencyCacheSaveOutput> {
    let mut saved = Vec::new();
    let mut missing = Vec::new();
    for path in &manifest.paths {
        let source = Path::new(remote_path).join(path).display().to_string();
        let archive = cache_archive_path(cache_path, path);
        let probe = format!(
            "if test -e {}; then printf yes; fi",
            shell::quote_arg(&source)
        );
        if run_shell_capture(&shell_command_for_runner(runner, &probe)?).is_none() {
            missing.push(path.clone());
            continue;
        }
        let command = format!(
            "mkdir -p {cache} {archive_parent} && tar -C {remote} -cf {archive} {path}",
            cache = shell::quote_arg(cache_path),
            archive_parent = shell::quote_arg(&parent_remote_path(&archive)),
            remote = shell::quote_arg(remote_path),
            archive = shell::quote_arg(&archive),
            path = shell::quote_arg(path),
        );
        run_shell_command(
            &shell_command_for_runner(runner, &command)?,
            "save runner dependency cache",
        )?;
        saved.push(path.clone());
    }
    let manifest_json = serde_json::to_string_pretty(manifest).unwrap_or_else(|_| "{}".to_string());
    let write_manifest = format!(
        "mkdir -p {cache} && printf %s {json} > {manifest}",
        cache = shell::quote_arg(cache_path),
        json = shell::quote_arg(&manifest_json),
        manifest = shell::quote_arg(
            &Path::new(cache_path)
                .join("manifest.json")
                .display()
                .to_string()
        ),
    );
    run_shell_command(
        &shell_command_for_runner(runner, &write_manifest)?,
        "write runner dependency cache manifest",
    )?;
    Ok(RunnerDependencyCacheSaveOutput {
        schema: DEPENDENCY_CACHE_SCHEMA,
        status: if saved.is_empty() { "empty" } else { "saved" }.to_string(),
        key: manifest.key.clone(),
        cache_path: cache_path.to_string(),
        saved_paths: saved,
        missing_paths: missing,
        manifest: manifest.clone(),
    })
}

fn validate_dependency_cache_spec(spec: &DependencyCacheSpec) -> Result<()> {
    if spec.step_id.trim().is_empty() || spec.paths.is_empty() {
        return Err(Error::validation_invalid_argument(
            "dependency_cache",
            "dependency cache requires a non-empty step_id and at least one path",
            None,
            None,
        ));
    }
    for path in spec
        .paths
        .iter()
        .chain(spec.lockfiles.iter())
        .chain(spec.package_metadata.iter())
    {
        normalize_relative_cache_path(path)?;
    }
    Ok(())
}

fn cache_key_file(
    local_path: &Path,
    role: &str,
    path: &str,
) -> Result<RunnerDependencyCacheKeyFile> {
    let normalized = normalize_relative_cache_path(path)?;
    let full = local_path.join(&normalized);
    if !full.is_file() {
        return Ok(RunnerDependencyCacheKeyFile {
            role: role.to_string(),
            path: normalized,
            status: "missing".to_string(),
            sha256: None,
        });
    }
    let bytes = std::fs::read(&full).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read dependency cache key file {}", full.display())),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(RunnerDependencyCacheKeyFile {
        role: role.to_string(),
        path: normalized,
        status: "present".to_string(),
        sha256: Some(hex(&hasher.finalize())),
    })
}

fn normalize_relative_cache_path(path: &str) -> Result<String> {
    let value = path.trim().trim_matches('/');
    if value.is_empty()
        || value.starts_with("../")
        || value.contains("/../")
        || Path::new(value).is_absolute()
    {
        return Err(Error::validation_invalid_argument(
            "dependency_cache",
            "dependency cache paths must be non-empty relative paths that stay inside the checkout",
            Some(path.to_string()),
            None,
        ));
    }
    Ok(value.to_string())
}

fn dependency_cache_path(runner: &Runner, remote_path: &str, key: &str) -> String {
    let root = runner
        .workspace_root
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| parent_remote_path(remote_path));
    format!("{}/_dependency_cache/{}", root.trim_end_matches('/'), key)
}

fn cache_archive_path(cache_path: &str, path: &str) -> String {
    PathBuf::from(cache_path)
        .join(format!("{}.tar", path.replace('/', "__")))
        .display()
        .to_string()
}

fn runner_label(runner: &Runner, key: &str, fallback: &str) -> String {
    runner
        .resources
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match runner.kind {
            RunnerKind::Local => fallback.to_string(),
            RunnerKind::Ssh => fallback.to_string(),
        })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependencyUpdateStatus {
    NotGit,
    DirtyNotUpdated,
    DirtyOverlayAllowed,
    NoUpstream,
    DetachedUnpinned,
    UpToDate,
    FastForwarded,
    PinnedRef,
    FetchFailedCachedUpToDate,
    FetchFailed,
    BehindAfterFetch,
}

impl DependencyUpdateStatus {
    fn label(self) -> &'static str {
        match self {
            Self::NotGit => "snapshotted",
            Self::DirtyNotUpdated => "dirty_not_updated",
            Self::DirtyOverlayAllowed => "snapshotted_dirty_overlay",
            Self::NoUpstream => "no_upstream",
            Self::DetachedUnpinned => "detached_unpinned",
            Self::UpToDate => "snapshotted_up_to_date",
            Self::FastForwarded => "snapshotted_fast_forwarded",
            Self::PinnedRef => "snapshotted_pinned_ref",
            Self::FetchFailedCachedUpToDate => "snapshotted_fetch_failed_cached_up_to_date",
            Self::FetchFailed => "fetch_failed_cached",
            Self::BehindAfterFetch => "behind_after_fetch",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::DirtyNotUpdated
                | Self::NoUpstream
                | Self::DetachedUnpinned
                | Self::FetchFailed
                | Self::BehindAfterFetch
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DependencyFreshness {
    status: DependencyUpdateStatus,
    branch: Option<String>,
    before_sha: Option<String>,
    after_sha: Option<String>,
    upstream_sha: Option<String>,
    upstream: Option<String>,
    pinned_ref: Option<String>,
    used_pinned_ref: bool,
}

impl DependencyFreshness {
    /// Build a freshness record for the common (non-pinned) case where both the
    /// before/after SHA derive from the same `HEAD` snapshot and no pinned ref is
    /// in play. Centralizes the repeated struct literal so each call site only
    /// names the parts that actually vary (status, branch, upstream metadata).
    fn at_head(
        status: DependencyUpdateStatus,
        branch: Option<String>,
        before: &str,
        upstream_sha: Option<String>,
        upstream: Option<String>,
    ) -> Self {
        DependencyFreshness {
            status,
            branch,
            before_sha: Some(before.to_string()),
            after_sha: Some(before.to_string()),
            upstream_sha,
            upstream,
            pinned_ref: None,
            used_pinned_ref: false,
        }
    }
}

fn ensure_git_dependency_fresh(
    local_path: &Path,
    pinned_ref: Option<&str>,
    allow_dirty: bool,
) -> Result<DependencyFreshness> {
    if !local_path.join(".git").exists() {
        return Ok(DependencyFreshness {
            status: DependencyUpdateStatus::NotGit,
            branch: None,
            before_sha: None,
            after_sha: None,
            upstream_sha: None,
            upstream: None,
            pinned_ref: pinned_ref.map(str::to_string),
            used_pinned_ref: pinned_ref.is_some(),
        });
    }

    let before = git_output(local_path, &["rev-parse", "HEAD"])?;
    let branch = git_output(local_path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
    if branch.as_deref() == Some("HEAD") && pinned_ref.is_none() {
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::DetachedUnpinned,
            branch,
            &before,
            None,
            None,
        );
        return Err(terminal_dependency_error(local_path, &freshness, None));
    }

    if let Some(pinned_ref) = pinned_ref.filter(|value| !value.trim().is_empty()) {
        return Ok(DependencyFreshness {
            status: DependencyUpdateStatus::PinnedRef,
            branch,
            before_sha: Some(before.clone()),
            after_sha: Some(before),
            upstream_sha: None,
            upstream: None,
            pinned_ref: Some(pinned_ref.to_string()),
            used_pinned_ref: true,
        });
    }

    let upstream = match git_output(
        local_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    ) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            let freshness = DependencyFreshness::at_head(
                DependencyUpdateStatus::NoUpstream,
                branch,
                &before,
                None,
                None,
            );
            return Err(terminal_dependency_error(local_path, &freshness, None));
        }
    };
    let remote = upstream.split('/').next().unwrap_or("").trim();
    if remote.is_empty() || remote == upstream {
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::NoUpstream,
            branch,
            &before,
            None,
            Some(upstream),
        );
        return Err(terminal_dependency_error(local_path, &freshness, None));
    }

    let fetch_error = run_git(local_path, &["fetch", "--prune", remote]).err();
    let upstream_head = git_output(local_path, &["rev-parse", "@{u}"]).ok();
    let status = git_output(local_path, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        // A dirty working tree (tracked changes and/or untracked files) is
        // refused by default so bench results are reproducible from clean git.
        // With an explicit override the dirty working tree is snapshotted as an
        // overlay: `materialize_snapshot` tars the working directory, so the
        // dirty files travel to the runner verbatim.
        if allow_dirty {
            return Ok(DependencyFreshness::at_head(
                DependencyUpdateStatus::DirtyOverlayAllowed,
                branch,
                &before,
                upstream_head,
                Some(upstream),
            ));
        }
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::DirtyNotUpdated,
            branch,
            &before,
            upstream_head,
            Some(upstream),
        );
        return Err(terminal_dependency_error(
            local_path,
            &freshness,
            fetch_error,
        ));
    }

    if fetch_error.is_some() && upstream_head.as_deref() == Some(before.as_str()) {
        return Ok(DependencyFreshness::at_head(
            DependencyUpdateStatus::FetchFailedCachedUpToDate,
            branch,
            &before,
            upstream_head,
            Some(upstream),
        ));
    }

    if fetch_error.is_some() {
        let freshness = DependencyFreshness::at_head(
            DependencyUpdateStatus::FetchFailed,
            branch,
            &before,
            upstream_head,
            Some(upstream),
        );
        return Err(terminal_dependency_error(
            local_path,
            &freshness,
            fetch_error,
        ));
    }

    if upstream_head.as_deref().is_some_and(|head| head != before) {
        run_git(local_path, &["merge", "--ff-only", "@{u}"])?;
    }
    let after = git_output(local_path, &["rev-parse", "HEAD"])?;
    let upstream_head = git_output(local_path, &["rev-parse", "@{u}"]).ok();
    let status = if upstream_head.as_deref().is_some_and(|head| head != after) {
        DependencyUpdateStatus::BehindAfterFetch
    } else if before == after {
        DependencyUpdateStatus::UpToDate
    } else {
        DependencyUpdateStatus::FastForwarded
    };
    let freshness = DependencyFreshness {
        status,
        branch,
        before_sha: Some(before),
        after_sha: Some(after),
        upstream_sha: upstream_head,
        upstream: Some(upstream),
        pinned_ref: None,
        used_pinned_ref: false,
    };

    if freshness.status.is_terminal() {
        return Err(terminal_dependency_error(local_path, &freshness, None));
    }

    Ok(freshness)
}

fn terminal_dependency_error(
    local_path: &Path,
    freshness: &DependencyFreshness,
    source_error: Option<Error>,
) -> Error {
    let mut hints = vec![
        "Update, rebase, or clean the dependency checkout before rerunning the Lab proof.".to_string(),
        "Use an explicit pinned ref in the rig/component dependency only when the stale checkout is intentional.".to_string(),
    ];
    if freshness.status == DependencyUpdateStatus::DirtyNotUpdated {
        hints.push(
            "Pass --allow-dirty-lab-workspace to snapshot the dirty working tree (tracked changes and untracked files) as an explicit overlay.".to_string(),
        );
        hints.push(
            "Or make the dirty checkout the primary bench workspace with --path so its working tree is snapshotted directly instead of as a clean git-only rig dependency.".to_string(),
        );
    }
    if freshness.status == DependencyUpdateStatus::DetachedUnpinned {
        hints.push(format!(
            "Use a branch-backed dependency checkout before Lab offload: git -C {} switch <branch>",
            shell_arg(&local_path.display().to_string())
        ));
        hints.push(
            "If this detached checkout is the component under test, create/select a branch-backed worktree and rerun the rig proof with --path <component-path>.".to_string(),
        );
        hints.push(
            "If the detached commit is intentional and reviewable, pin the rig component dependency with an explicit ref.".to_string(),
        );
    }
    if freshness.status == DependencyUpdateStatus::NoUpstream {
        hints.push(format!(
            "Set an upstream for the dependency branch before Lab offload: git -C {} branch --set-upstream-to=<remote>/<branch>",
            shell_arg(&local_path.display().to_string())
        ));
        hints.push(
            "Or use a branch-backed worktree with an upstream and pass it as the rig component --path when it is the component under test.".to_string(),
        );
    }
    if let Some(error) = &source_error {
        hints.push(format!("Fetch failure: {}", error.message));
    }
    Error::validation_invalid_argument(
        "rig_component_dependency",
        format!(
            "Lab offload refused stale or ambiguous git dependency `{}` with status `{}`",
            local_path.display(),
            freshness.status.label()
        ),
        Some(
            serde_json::json!({
                "local_path": local_path.display().to_string(),
                "status": freshness.status.label(),
                "branch": freshness.branch.as_deref(),
                "before_sha": freshness.before_sha.as_deref(),
                "after_sha": freshness.after_sha.as_deref(),
                "upstream": freshness.upstream.as_deref(),
                "upstream_sha": freshness.upstream_sha.as_deref(),
                "pinned_ref": freshness.pinned_ref.as_deref(),
                "used_pinned_ref": freshness.used_pinned_ref,
            })
            .to_string(),
        ),
        Some(hints),
    )
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn run_git(local_path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(local_path)
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some("run git".to_string())))?;
    if output.status.success() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "rig_component_dependency",
        format!(
            "git dependency auto-update failed while running git {}",
            args.join(" ")
        ),
        Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        Some(vec![
            "Commit or stash dependency changes before Lab offload.".to_string(),
            "If the dependency branch diverged, update or rebase it manually before rerunning."
                .to_string(),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use crate::core::engine::shell;

    use super::{
        cache_archive_path, dependency_cache_manifest, ensure_git_dependency_fresh,
        materialize_git_dependency, restore_dependency_cache_paths, save_dependency_cache_paths,
        DependencyUpdateStatus, Runner, RunnerDependencyCacheManifest,
        RunnerGitDependencyMaterializationOptions, RunnerKind, DEPENDENCY_CACHE_SCHEMA,
    };

    #[test]
    fn materialized_git_dependency_preserves_canonical_git_provenance() {
        // Regression for #4314: the default snapshot excludes strip `.git`, so a
        // plain snapshot lands a runner-side component path with no git
        // provenance and canonical trace preflight rejects it as `not-git`
        // before the workload starts. Materializing a real git checkout must
        // seed a synthetic git checkout on the runner so the materialized path
        // is a valid git work tree with a committed HEAD.
        crate::test_support::with_isolated_home(|_| {
            let fixture = GitFixture::new();
            fixture.commit_file("initial.txt", "initial");
            fixture.push();
            let checkout = fixture.clone_checkout();

            let runner_root = tempfile::tempdir().expect("runner root");
            crate::core::runner::create(
                &format!(
                    r#"{{"id":"lab-local-git-dependency","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let runner =
                crate::core::runner::load("lab-local-git-dependency").expect("load runner");

            let remote_path = runner_root
                .path()
                .join("materialized-dependency")
                .display()
                .to_string();
            let output = materialize_git_dependency(
                &runner,
                RunnerGitDependencyMaterializationOptions {
                    local_path: checkout.path().display().to_string(),
                    remote_path: remote_path.clone(),
                    remote_url: None,
                    required_subpath: None,
                    pinned_ref: None,
                    allow_dirty: false,
                    dependency_cache: None,
                    component_ref: None,
                },
            )
            .expect("materialize git dependency");

            assert_eq!(output.remote_path, remote_path);
            let remote = Path::new(&remote_path);
            // Canonical provenance preserved: the materialized path is a real git
            // work tree with a resolvable HEAD, so trace preflight no longer
            // rejects it as `not-git`.
            assert_eq!(
                git_output(remote, &["rev-parse", "--is-inside-work-tree"]),
                "true"
            );
            assert!(!git_output(remote, &["rev-parse", "HEAD"]).is_empty());
            // Working tree is a clean committed snapshot, not a dirty checkout.
            assert!(git_output(remote, &["status", "--porcelain=v1"]).is_empty());
        });
    }

    #[test]
    fn auto_update_clean_dependency_fast_forwards_to_upstream() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        fixture.commit_file("next.txt", "next");
        fixture.push();
        let expected = fixture.head();

        let freshness =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect("auto update");

        assert_eq!(freshness.status, DependencyUpdateStatus::FastForwarded);
        assert_ne!(before, expected);
        assert_eq!(freshness.before_sha.as_deref(), Some(before.as_str()));
        assert_eq!(freshness.after_sha.as_deref(), Some(expected.as_str()));
        assert_eq!(freshness.upstream_sha.as_deref(), Some(expected.as_str()));
        assert_eq!(
            git_output(checkout.path(), &["rev-parse", "HEAD"]),
            expected
        );
    }

    #[test]
    fn dirty_dependency_fails_before_snapshotting() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        fs::write(checkout.path().join("dirty.txt"), "dirty").expect("write dirty file");
        fixture.commit_file("next.txt", "next");
        fixture.push();

        let err =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect_err("dirty fails");

        assert!(err.message.contains("dirty_not_updated"));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn detached_dependency_without_pinned_ref_fails() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let head = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        run_git(checkout.path(), &["checkout", "--detach", &head]);

        let err =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect_err("detached fails");

        assert!(err.message.contains("detached_unpinned"));
        let hints = err.details["tried"]
            .as_array()
            .expect("tried hints")
            .iter()
            .filter_map(|hint| hint.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(hints.contains("git -C"));
        assert!(hints.contains("switch <branch>"));
        assert!(hints.contains("--path <component-path>"));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn dependency_without_upstream_fails_before_snapshotting() {
        let repo = tempfile::tempdir().expect("repo");
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        run_git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(repo.path().join("initial.txt"), "initial").expect("write file");
        run_git(repo.path(), &["add", "initial.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        let err =
            ensure_git_dependency_fresh(repo.path(), None, false).expect_err("no upstream fails");

        assert!(err.message.contains("no_upstream"));
    }

    #[test]
    fn explicit_pinned_ref_allows_detached_dependency() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let head = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        run_git(checkout.path(), &["checkout", "--detach", &head]);

        let freshness =
            ensure_git_dependency_fresh(checkout.path(), Some(&head), false).expect("pinned");

        assert_eq!(freshness.status, DependencyUpdateStatus::PinnedRef);
        assert!(freshness.used_pinned_ref);
        assert_eq!(freshness.pinned_ref.as_deref(), Some(head.as_str()));
    }

    #[test]
    fn fetch_failure_uses_cached_upstream_when_checkout_matches() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        let missing_remote = checkout.path().join("missing-remote.git");
        run_git(
            checkout.path(),
            &[
                "remote",
                "set-url",
                "origin",
                missing_remote.to_str().expect("missing remote path"),
            ],
        );

        let freshness =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect("cached fallback");

        assert_eq!(
            freshness.status,
            DependencyUpdateStatus::FetchFailedCachedUpToDate
        );
        assert_eq!(freshness.before_sha.as_deref(), Some(before.as_str()));
        assert_eq!(freshness.after_sha.as_deref(), Some(before.as_str()));
        assert_eq!(freshness.upstream_sha.as_deref(), Some(before.as_str()));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn fetch_failure_is_terminal_when_cached_upstream_differs() {
        let fixture = GitFixture::new();
        fixture.commit_file("initial.txt", "initial");
        fixture.push();
        fixture.commit_file("next.txt", "next");
        fixture.push();

        let checkout = fixture.clone_checkout();
        let upstream = git_output(checkout.path(), &["rev-parse", "@{u}"]);
        run_git(checkout.path(), &["reset", "--hard", "HEAD~1"]);
        let before = git_output(checkout.path(), &["rev-parse", "HEAD"]);
        let missing_remote = checkout.path().join("missing-remote.git");
        run_git(
            checkout.path(),
            &[
                "remote",
                "set-url",
                "origin",
                missing_remote.to_str().expect("missing remote path"),
            ],
        );

        let err =
            ensure_git_dependency_fresh(checkout.path(), None, false).expect_err("fetch fails");

        assert_ne!(before, upstream);
        assert!(err.message.contains("fetch_failed_cached"));
        assert_eq!(git_output(checkout.path(), &["rev-parse", "HEAD"]), before);
    }

    #[test]
    fn dependency_cache_key_changes_with_declared_inputs() {
        let root = tempfile::tempdir().expect("root");
        fs::write(root.path().join("lock.txt"), "one").expect("write lock");
        fs::write(root.path().join("package.meta"), "meta").expect("write metadata");
        let mut runner = test_runner(tempfile::tempdir().expect("runner root").path());
        runner
            .resources
            .insert("os".to_string(), serde_json::json!("linux"));
        runner
            .resources
            .insert("arch".to_string(), serde_json::json!("x86_64"));
        let spec = crate::core::rig::spec::DependencyCacheSpec {
            step_id: "deps".to_string(),
            paths: vec!["vendor/cache".to_string()],
            lockfiles: vec!["lock.txt".to_string()],
            package_metadata: vec!["package.meta".to_string()],
        };

        let first =
            dependency_cache_manifest(&runner, root.path(), &spec, "abc").expect("manifest");
        fs::write(root.path().join("lock.txt"), "two").expect("update lock");
        let second =
            dependency_cache_manifest(&runner, root.path(), &spec, "abc").expect("manifest");

        assert_ne!(first.key, second.key);
        assert_eq!(first.step_id, "deps");
        assert_eq!(first.runner_os, "linux");
        assert_eq!(first.runner_arch, "x86_64");
        assert_eq!(first.key_files.len(), 2);
    }

    #[test]
    fn dependency_cache_restore_reports_miss_and_hit() {
        let runner_root = tempfile::tempdir().expect("runner root");
        let remote = runner_root.path().join("checkout");
        let cache = runner_root.path().join("cache");
        fs::create_dir_all(&remote).expect("remote");
        let runner = test_runner(runner_root.path());
        let manifest = test_cache_manifest("key", vec!["deps".to_string()]);

        let miss = restore_dependency_cache_paths(
            &runner,
            &remote.display().to_string(),
            &cache.display().to_string(),
            &manifest,
        )
        .expect("restore miss");
        assert_eq!(miss.status, "miss");
        assert_eq!(miss.missing_paths, vec!["deps"]);

        fs::create_dir_all(cache.join("unused")).ok();
        fs::create_dir_all(runner_root.path().join("source/deps")).expect("source deps");
        fs::write(runner_root.path().join("source/deps/file.txt"), "cached").expect("cached file");
        let archive = cache_archive_path(&cache.display().to_string(), "deps");
        run_shell(&format!(
            "mkdir -p {} && tar -C {} -cf {} deps",
            shell::quote_arg(&cache.display().to_string()),
            shell::quote_arg(&runner_root.path().join("source").display().to_string()),
            shell::quote_arg(&archive),
        ));

        let hit = restore_dependency_cache_paths(
            &runner,
            &remote.display().to_string(),
            &cache.display().to_string(),
            &manifest,
        )
        .expect("restore hit");
        assert_eq!(hit.status, "hit");
        assert_eq!(hit.restored_paths, vec!["deps"]);
        assert_eq!(
            fs::read_to_string(remote.join("deps/file.txt")).expect("restored file"),
            "cached"
        );
    }

    #[test]
    fn dependency_cache_save_writes_manifest_shape() {
        let runner_root = tempfile::tempdir().expect("runner root");
        let remote = runner_root.path().join("checkout");
        let cache = runner_root.path().join("cache");
        fs::create_dir_all(remote.join("deps")).expect("deps");
        fs::write(remote.join("deps/file.txt"), "saved").expect("saved file");
        let runner = test_runner(runner_root.path());
        let manifest =
            test_cache_manifest("key-save", vec!["deps".to_string(), "missing".to_string()]);

        let saved = save_dependency_cache_paths(
            &runner,
            &remote.display().to_string(),
            &cache.display().to_string(),
            &manifest,
        )
        .expect("save cache");

        assert_eq!(saved.status, "saved");
        assert_eq!(saved.saved_paths, vec!["deps"]);
        assert_eq!(saved.missing_paths, vec!["missing"]);
        assert!(Path::new(&cache_archive_path(&cache.display().to_string(), "deps")).is_file());
        let manifest_json = fs::read_to_string(cache.join("manifest.json")).expect("manifest json");
        let value: serde_json::Value = serde_json::from_str(&manifest_json).expect("json");
        assert_eq!(value["schema"], DEPENDENCY_CACHE_SCHEMA);
        assert_eq!(value["key"], "key-save");
        assert_eq!(value["paths"].as_array().expect("paths").len(), 2);
    }

    struct GitFixture {
        remote: tempfile::TempDir,
        work: tempfile::TempDir,
    }

    impl GitFixture {
        fn new() -> Self {
            let remote = tempfile::tempdir().expect("remote");
            run_git(remote.path(), &["init", "--bare"]);

            let work = tempfile::tempdir().expect("work");
            run_git(work.path(), &["init"]);
            run_git(
                work.path(),
                &["config", "user.email", "homeboy@example.test"],
            );
            run_git(work.path(), &["config", "user.name", "Homeboy Test"]);
            run_git(
                work.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.path().to_str().expect("remote path"),
                ],
            );

            Self { remote, work }
        }

        fn commit_file(&self, name: &str, contents: &str) {
            fs::write(self.work.path().join(name), contents).expect("write file");
            run_git(self.work.path(), &["add", name]);
            run_git(self.work.path(), &["commit", "-m", name]);
        }

        fn push(&self) {
            run_git(self.work.path(), &["push", "-u", "origin", "HEAD:main"]);
        }

        fn head(&self) -> String {
            git_output(self.work.path(), &["rev-parse", "HEAD"])
        }

        fn clone_checkout(&self) -> tempfile::TempDir {
            let checkout = tempfile::tempdir().expect("checkout");
            run_git(
                Path::new("/"),
                &[
                    "clone",
                    self.remote.path().to_str().expect("remote path"),
                    checkout.path().to_str().expect("checkout path"),
                ],
            );
            run_git(checkout.path(), &["checkout", "main"]);
            checkout
        }
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn test_runner(workspace_root: &Path) -> Runner {
        Runner {
            id: "test-runner".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some(workspace_root.display().to_string()),
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        }
    }

    fn test_cache_manifest(key: &str, paths: Vec<String>) -> RunnerDependencyCacheManifest {
        RunnerDependencyCacheManifest {
            schema: DEPENDENCY_CACHE_SCHEMA,
            key: key.to_string(),
            step_id: "deps".to_string(),
            component_ref: "abc".to_string(),
            runner_os: "linux".to_string(),
            runner_arch: "x86_64".to_string(),
            key_files: Vec::new(),
            paths,
        }
    }

    fn run_shell(command: &str) {
        let output = Command::new("sh")
            .args(["-c", command])
            .output()
            .expect("run shell");
        assert!(
            output.status.success(),
            "shell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
