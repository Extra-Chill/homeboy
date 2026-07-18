use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{fs, io::Read};

use sha2::{Digest, Sha256};

use homeboy_core::artifact_inputs::ResolvedArtifactInput;
use homeboy_core::component::Component;
use homeboy_core::config;
use homeboy_core::error::Result;
use homeboy_core::is_zero_u32;
use homeboy_core::paths as base_path;
use homeboy_core::phase_timing::PhaseTimingReport;
use homeboy_core::project::Project;

use super::path_roots::resolve_effective_remote_path;

/// Parse bulk component IDs from a JSON spec.
pub fn parse_bulk_component_ids(json_spec: &str) -> Result<Vec<String>> {
    let input = config::parse_bulk_ids(json_spec)?;
    Ok(input.component_ids)
}

pub struct DeployResult {
    pub success: bool,
    pub exit_code: i32,
    pub error: Option<String>,
    pub effect: Option<DeployEffect>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployEffect {
    pub remote_path: String,
    pub artifact_path: Option<String>,
    pub verified: bool,
}

impl DeployResult {
    pub(super) fn success(exit_code: i32) -> Self {
        Self {
            success: true,
            exit_code,
            error: None,
            effect: None,
        }
    }

    pub(super) fn failure(exit_code: i32, error: String) -> Self {
        Self {
            success: false,
            exit_code,
            error: Some(error),
            effect: None,
        }
    }

    pub(super) fn with_effect(mut self, effect: DeployEffect) -> Self {
        self.effect = Some(effect);
        self
    }
}

#[derive(Clone)]
pub struct DeployConfig {
    pub component_ids: Vec<String>,
    pub all: bool,
    pub outdated: bool,
    pub behind_upstream: bool,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    /// Skip build if artifact already exists (used by release --deploy)
    pub skip_build: bool,
    /// Keep build dependencies (skip cleanup even when auto_cleanup is enabled)
    pub keep_deps: bool,
    /// Skip dependency hydration before building an exact-ref checkout.
    pub skip_deps_hydration: bool,
    /// Assert expected version before deploying (abort if mismatch)
    pub expected_version: Option<String>,
    /// Skip auto-pulling latest changes before deploy
    pub no_pull: bool,
    /// Permit a local build from a checkout known to be behind its upstream.
    pub allow_stale_source: bool,
    /// Permit a local build whose semantic version is older than the deployed version.
    pub allow_downgrade: bool,
    /// Deploy from current branch HEAD instead of latest tag
    pub head: bool,
    /// Resolve and deploy this Git ref from each component's declared repository.
    pub requested_ref: Option<String>,
    /// Force tag-based deploy, ignoring any reusable build artifacts
    pub tagged: bool,
    /// An immutable artifact prepared by an upstream workflow.
    pub prepared_artifact: Option<PreparedDeployArtifact>,
    /// Resume a durable multi-target deploy run after exact identity validation.
    pub resume_run_id: Option<String>,
}

impl DeployConfig {
    /// Build the common status/check configuration: inspect every component,
    /// avoid mutating local checkouts, and compare the current HEAD.
    pub fn check_all_no_pull_head() -> Self {
        Self {
            component_ids: vec![],
            all: true,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: true,
            force: false,
            skip_build: true,
            keep_deps: false,
            skip_deps_hydration: false,
            expected_version: None,
            no_pull: true,
            allow_stale_source: false,
            allow_downgrade: false,
            head: true,
            requested_ref: None,
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        }
    }
}

/// A durable, pre-built payload supplied by an upstream workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedDeployArtifact {
    pub component_id: String,
    pub path: String,
    pub durable_path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub version: String,
    pub tag: String,
    pub source_commit: String,
}

impl PreparedDeployArtifact {
    pub(crate) fn effective_path(&self) -> &str {
        &self.durable_path
    }

    pub(crate) fn validate(
        &self,
        component_id: &str,
        expected_version: Option<&str>,
    ) -> Result<()> {
        if self.component_id != component_id {
            return Err(homeboy_core::error::Error::validation_invalid_argument(
                "prepared_artifact.component_id",
                format!(
                    "Prepared artifact is for '{}' rather than '{}'",
                    self.component_id, component_id
                ),
                None,
                None,
            ));
        }
        if let Some(expected_version) = expected_version {
            if expected_version != self.version {
                return Err(homeboy_core::error::Error::validation_invalid_argument(
                    "prepared_artifact.version",
                    format!(
                        "Prepared artifact version '{}' does not match expected version '{}'",
                        self.version, expected_version
                    ),
                    None,
                    None,
                ));
            }
        }

        let path = Path::new(self.effective_path());
        let metadata = fs::metadata(path).map_err(|error| {
            homeboy_core::error::Error::internal_io(
                format!(
                    "Prepared artifact '{}' is unavailable: {}",
                    path.display(),
                    error
                ),
                Some(path.display().to_string()),
            )
        })?;
        if !metadata.is_file() {
            return Err(homeboy_core::error::Error::validation_invalid_argument(
                "prepared_artifact.durable_path",
                format!("Prepared artifact '{}' is not a file", path.display()),
                Some(path.display().to_string()),
                None,
            ));
        }
        if metadata.len() != self.size_bytes {
            return Err(homeboy_core::error::Error::validation_invalid_argument(
                "prepared_artifact.size_bytes",
                format!(
                    "Prepared artifact size changed: expected {}, found {}",
                    self.size_bytes,
                    metadata.len()
                ),
                Some(path.display().to_string()),
                None,
            ));
        }
        let actual_sha256 = sha256_file(path)?;
        if actual_sha256 != self.sha256 {
            return Err(homeboy_core::error::Error::validation_invalid_argument(
                "prepared_artifact.sha256",
                format!(
                    "Prepared artifact SHA-256 changed: expected {}, found {}",
                    self.sha256, actual_sha256
                ),
                Some(path.display().to_string()),
                None,
            ));
        }
        Ok(())
    }

    /// An exact ref can reuse a prepared artifact only when its immutable source
    /// identity was recorded at that same commit.
    pub(crate) fn validate_exact_source(
        &self,
        component_id: &str,
        expected_version: Option<&str>,
        resolved_sha: &str,
    ) -> Result<()> {
        self.validate(component_id, expected_version)?;
        if self.source_commit != resolved_sha {
            return Err(homeboy_core::error::Error::validation_invalid_argument(
                "prepared_artifact.source_commit",
                format!(
                    "Prepared artifact source commit '{}' does not match exact ref commit '{}'",
                    self.source_commit, resolved_sha
                ),
                None,
                None,
            ));
        }
        Ok(())
    }
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|error| {
        homeboy_core::error::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            homeboy_core::error::Error::internal_io(
                error.to_string(),
                Some(path.display().to_string()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployArtifactSource {
    ReleaseAsset,
    LocalBuild,
    Prepared,
}

/// Where the payload that was deployed actually came from.
///
/// Unlike [`DeployArtifactSource`] (which only distinguishes a downloaded release
/// asset from a local build for artifact strategies), this covers every deploy
/// strategy so the deploy result is explicit and consistent about build provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildSource {
    /// A fresh build ran locally against the checked-out source tree.
    FreshBuild,
    /// An existing local build artifact was reused without rebuilding (`--skip-build`).
    ReusedArtifact,
    /// A prebuilt release asset was downloaded instead of building locally.
    DownloadedRelease,
    /// A durable artifact supplied by an upstream workflow.
    PreparedArtifact,
    /// Committed source was pushed directly via the `git` deploy strategy.
    GitPush,
    /// Source files were copied directly via the `file` deploy strategy.
    FileCopy,
}

/// Content identity of a deployed artifact file, so stale artifacts are detectable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactIdentity {
    /// Path to the artifact that was deployed.
    pub path: String,
    /// Size of the artifact in bytes (absent for directory artifacts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// SHA-256 of the artifact contents (best-effort; absent for directories).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Last-modified time of the artifact, in seconds since the Unix epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_unix: Option<i64>,
}

/// Explicit, strategy-complete build provenance for a single component deploy.
///
/// Answers, for every deploy and regardless of strategy: which source/ref produced
/// the payload, whether a fresh build actually ran, whether the built source tree
/// was dirty, and the identity of the artifact that shipped. This makes the
/// otherwise-implicit "working tree vs. committed ref vs. reused artifact" choice
/// explicit so stale artifacts cannot ship unnoticed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildProvenance {
    /// Where the deployed payload came from.
    pub source: BuildSource,
    /// Whether a fresh build ran for this deploy (vs. a reused or downloaded
    /// artifact, or a strategy that ships source directly).
    pub build_ran: bool,
    /// The git ref the payload was built/deployed from (tag, or `"<branch> (HEAD)"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub built_from_ref: Option<String>,
    /// The commit (HEAD) of the source tree the payload was built/deployed from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub built_from_commit: Option<String>,
    /// Whether the built source tree had uncommitted changes (excluding generated
    /// build artifacts). `None` when not applicable (e.g. a downloaded release).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_tree_dirty: Option<bool>,
    /// Identity of the deployed artifact, when the strategy produces an artifact file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_identity: Option<ArtifactIdentity>,
}

// DeployReason and ComponentStatus moved DOWN to homeboy-release-contract so
// core's fleet/project/context status mechanics can reference them without a
// cycle. Re-exported here so the deploy/release code keeps its paths.
pub use homeboy_release_contract::{ComponentStatus, DeployReason};

/// Compare the configured source version with the version observed on the target.
///
/// This deliberately describes deployment health only. Callers may layer source
/// freshness checks on top when both versions identify the same deployed release.
pub fn compare_deployed_versions(
    local_version: Option<&str>,
    remote_version: Option<&str>,
) -> ComponentStatus {
    match (local_version, remote_version) {
        (Some(local), Some(remote)) if local == remote => ComponentStatus::UpToDate,
        (Some(local), Some(remote)) => {
            let parse = |version: &str| semver::Version::parse(version.trim_start_matches('v'));
            match (parse(local), parse(remote)) {
                (Ok(local), Ok(remote)) if local > remote => ComponentStatus::NeedsUpdate,
                (Ok(local), Ok(remote)) if local < remote => ComponentStatus::BehindRemote,
                (Ok(_), Ok(_)) => ComponentStatus::UpToDate,
                _ => ComponentStatus::Unknown,
            }
        }
        _ => ComponentStatus::Unknown,
    }
}

// ReleaseState, ReleaseStateStatus, ReleaseStateBuckets moved DOWN to
// homeboy-release-contract (see the DeployReason/ComponentStatus note above).
pub use homeboy_release_contract::{ReleaseState, ReleaseStateBuckets, ReleaseStateStatus};

/// Result for a single component deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ComponentDeployResult {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_reason: Option<DeployReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_status: Option<ComponentStatus>,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_worktree: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind_upstream: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_source: Option<DeployArtifactSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<ResolvedArtifactInput>,
    pub remote_path: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ReleaseState>,
    /// The git ref (tag or branch) that was built and deployed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployed_ref: Option<String>,
    /// Operator-provided exact source selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    /// Immutable commit selected before packaging.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_sha: Option<String>,
    /// Declared repository used to resolve the requested ref.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Whether exact-ref identity came from the checkout or its configured remote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_mode: Option<String>,
    /// Explicit build provenance: source/ref, whether a fresh build ran, and the
    /// identity of the artifact that shipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_provenance: Option<BuildProvenance>,
    /// Identity of an upstream-prepared artifact, when used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared_artifact: Option<PreparedDeployArtifact>,
}

impl ComponentDeployResult {
    pub(super) fn new(component: &Component, base_path: &str) -> Self {
        Self {
            id: component.id.clone(),
            status: String::new(),
            deploy_reason: None,
            component_status: None,
            local_version: None,
            remote_version: None,
            local_path: Some(component.local_path.clone()),
            git_branch: None,
            git_head: None,
            upstream_branch: None,
            upstream_head: None,
            is_worktree: None,
            behind_upstream: None,
            warnings: Vec::new(),
            error: None,
            artifact_path: component.build_artifact.clone(),
            artifact_source: None,
            artifact_inputs: Vec::new(),
            remote_path: base_path::join_remote_path(Some(base_path), &component.remote_path).ok(),
            build_exit_code: None,
            deploy_exit_code: None,
            release_state: None,
            deployed_ref: None,
            requested_ref: None,
            resolved_sha: None,
            source: None,
            resolution_mode: None,
            build_provenance: None,
            prepared_artifact: None,
        }
    }

    pub(super) fn new_for_project(
        component: &Component,
        project: &Project,
        base_path: &str,
    ) -> Self {
        let mut result = Self::new(component, base_path);
        result.remote_path = resolve_effective_remote_path(project, component, base_path).ok();
        result
    }

    /// Shorthand for the common failure pattern: status="failed" + versions + error.
    pub(super) fn failed(
        component: &Component,
        base_path: &str,
        local_version: Option<String>,
        remote_version: Option<String>,
        error: String,
    ) -> Self {
        Self::new(component, base_path)
            .with_status("failed")
            .with_versions(local_version, remote_version)
            .with_error(error)
    }

    pub(super) fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub(super) fn with_versions(mut self, local: Option<String>, remote: Option<String>) -> Self {
        self.local_version = local;
        self.remote_version = remote;
        self
    }

    pub(super) fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    pub(super) fn with_build_exit_code(mut self, code: Option<i32>) -> Self {
        self.build_exit_code = code;
        self
    }

    pub(super) fn with_deploy_exit_code(mut self, code: Option<i32>) -> Self {
        self.deploy_exit_code = code;
        self
    }

    pub(super) fn with_artifact_path(mut self, path: Option<String>) -> Self {
        self.artifact_path = path;
        self
    }

    pub(super) fn with_artifact_source(mut self, source: DeployArtifactSource) -> Self {
        self.artifact_source = Some(source);
        self
    }

    pub(super) fn with_prepared_artifact(mut self, artifact: PreparedDeployArtifact) -> Self {
        self.prepared_artifact = Some(artifact);
        self
    }

    pub(super) fn with_component_status(mut self, status: ComponentStatus) -> Self {
        self.component_status = Some(status);
        self
    }

    pub(super) fn with_remote_path(mut self, path: String) -> Self {
        self.remote_path = Some(path);
        self
    }

    pub(super) fn with_artifact_inputs(mut self, inputs: Vec<ResolvedArtifactInput>) -> Self {
        self.artifact_inputs = inputs;
        self
    }

    pub(super) fn with_release_state(mut self, state: ReleaseState) -> Self {
        self.release_state = Some(state);
        self
    }

    pub(super) fn with_deployed_ref(mut self, git_ref: String) -> Self {
        self.deployed_ref = Some(git_ref);
        self
    }

    pub(super) fn with_exact_ref_identity(
        mut self,
        requested_ref: &str,
        resolved_sha: &str,
        source: &str,
        resolution_mode: &str,
    ) -> Self {
        self.requested_ref = Some(requested_ref.to_string());
        self.resolved_sha = Some(resolved_sha.to_string());
        self.source = Some(source.to_string());
        self.resolution_mode = Some(resolution_mode.to_string());
        self.deployed_ref = Some(requested_ref.to_string());
        self.git_head = Some(resolved_sha.to_string());
        self.local_path = Some(source.to_string());
        self
    }

    pub(super) fn with_build_provenance(mut self, provenance: BuildProvenance) -> Self {
        self.build_provenance = Some(provenance);
        self
    }

    pub(super) fn with_source_identity(mut self, component: &Component, head_deploy: bool) -> Self {
        let path = Path::new(&component.local_path);
        self.local_path = Some(component.local_path.clone());
        self.git_branch = git_output(path, &["branch", "--show-current"]);
        self.git_head = git_output(path, &["rev-parse", "HEAD"]);
        self.upstream_branch = git_output(path, &["rev-parse", "--abbrev-ref", "@{upstream}"])
            .or_else(|| homeboy_core::git::default_remote_branch(path));
        self.upstream_head = self
            .upstream_branch
            .as_deref()
            .and_then(|upstream| git_output(path, &["rev-parse", upstream]));
        self.is_worktree = detect_linked_worktree(path);
        self.behind_upstream = self
            .upstream_branch
            .as_deref()
            .and_then(|upstream| {
                git_output(
                    path,
                    &[
                        "rev-list",
                        "--left-only",
                        "--count",
                        &format!("{upstream}...HEAD"),
                    ],
                )
            })
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|count| *count > 0);

        if self.git_head.is_some() && self.git_branch.is_none() {
            self.warnings.push(
                "configured_source_detached: source checkout is detached; deploy readiness is comparing against the remote default branch"
                    .to_string(),
            );
        }

        if head_deploy && self.is_worktree == Some(true) {
            self.warnings.push(
                "--head deploy will use a non-primary git worktree; confirm this is the intended source checkout"
                    .to_string(),
            );
        }

        if let Some(count) = self.behind_upstream {
            self.warnings.push(format!(
                "configured_source_behind_upstream: source checkout is {count} commit(s) behind {}",
                self.upstream_branch.as_deref().unwrap_or("upstream")
            ));
        }

        self
    }
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!value.is_empty()).then_some(value)
        })
}

fn detect_linked_worktree(path: &Path) -> Option<bool> {
    let git_dir = git_output(path, &["rev-parse", "--path-format=absolute", "--git-dir"])?;
    let common_dir = git_output(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Some(git_dir != common_dir)
}

#[cfg(test)]
mod tests {
    use super::{
        compare_deployed_versions, parse_bulk_component_ids, ComponentDeployResult,
        ComponentStatus, DeployConfig, DeployResult, PreparedDeployArtifact, ReleaseState,
        ReleaseStateStatus,
    };
    use homeboy_core::component::{Component, ScopedExtensionConfig};
    use homeboy_core::project::Project;
    use homeboy_core::test_support::with_isolated_home;
    use homeboy_extension::{DeployCapability, ExtensionManifest, RemotePathRootRule};
    use std::collections::HashMap;

    fn component() -> Component {
        Component::new(
            "fixture".to_string(),
            "/tmp/fixture".to_string(),
            "wp-content/plugins/fixture".to_string(),
            None,
        )
    }

    fn component_with_extension() -> Component {
        let mut component = component();
        component.extensions = Some(HashMap::from([(
            "wordpress".to_string(),
            ScopedExtensionConfig::default(),
        )]));
        component
    }

    fn install_wordpress_extension() {
        homeboy_extension::save_manifest(&ExtensionManifest {
            id: "wordpress".to_string(),
            name: "WordPress".to_string(),
            version: "1.0.0".to_string(),
            deploy: Some(DeployCapability {
                verifications: Vec::new(),
                overrides: Vec::new(),
                protected_path_suffixes: Vec::new(),
                owner_hints: Vec::new(),
                archive_install: Vec::new(),
                remote_path_inference: Vec::new(),
                path_roots: vec![RemotePathRootRule {
                    path_prefix: "wp-content".to_string(),
                    root: "wp_content".to_string(),
                    strip_prefix: true,
                    detect_command: None,
                }],
                version_patterns: Vec::new(),
                since_tag: None,
            }),
            ..serde_json::from_value(serde_json::json!({
                "name": "WordPress",
                "version": "1.0.0"
            }))
            .expect("manifest")
        })
        .expect("save extension");
    }

    fn deploy_result() -> ComponentDeployResult {
        ComponentDeployResult::new(&component(), "/var/www/example")
    }

    fn release_state() -> ReleaseState {
        ReleaseState {
            commits_since_version: 1,
            code_commits: 1,
            docs_only_commits: 0,
            has_uncommitted_changes: false,
            baseline_ref: Some("v1.0.0".to_string()),
            baseline_warning: None,
        }
    }

    #[test]
    fn check_all_no_pull_head_config_matches_status_check_contract() {
        let config = DeployConfig::check_all_no_pull_head();

        assert!(config.component_ids.is_empty());
        assert!(config.all);
        assert!(!config.outdated);
        assert!(!config.behind_upstream);
        assert!(!config.dry_run);
        assert!(config.check);
        assert!(!config.force);
        assert!(config.skip_build);
        assert!(!config.keep_deps);
        assert_eq!(config.expected_version, None);
        assert!(config.no_pull);
        assert!(config.head);
        assert!(!config.tagged);
    }

    #[test]
    fn test_parse_bulk_component_ids() {
        let parsed = parse_bulk_component_ids(r#"["api","web"]"#).expect("parse ids");

        assert_eq!(parsed, vec!["api", "web"]);
    }

    #[test]
    fn test_success() {
        let result = DeployResult::success(0);

        assert!(result.success);
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.error, None);
    }

    #[test]
    fn test_failure() {
        let result = DeployResult::failure(2, "boom".to_string());

        assert!(!result.success);
        assert_eq!(result.exit_code, 2);
        assert_eq!(result.error.as_deref(), Some("boom"));
    }

    #[test]
    fn test_failed() {
        let result = ComponentDeployResult::failed(
            &component(),
            "/var/www/example",
            Some("1.0.0".to_string()),
            Some("0.9.0".to_string()),
            "deploy failed".to_string(),
        );

        assert_eq!(result.status, "failed");
        assert_eq!(result.local_version.as_deref(), Some("1.0.0"));
        assert_eq!(result.remote_version.as_deref(), Some("0.9.0"));
        assert_eq!(result.error.as_deref(), Some("deploy failed"));
    }

    #[test]
    fn test_with_status() {
        let result = deploy_result().with_status("skipped");

        assert_eq!(result.status, "skipped");
    }

    #[test]
    fn test_with_versions() {
        let result = deploy_result().with_versions(Some("1.0.0".into()), Some("0.9.0".into()));

        assert_eq!(result.local_version.as_deref(), Some("1.0.0"));
        assert_eq!(result.remote_version.as_deref(), Some("0.9.0"));
    }

    #[test]
    fn test_with_error() {
        let result = deploy_result().with_error("broken".to_string());

        assert_eq!(result.error.as_deref(), Some("broken"));
    }

    #[test]
    fn test_with_build_exit_code() {
        let result = deploy_result().with_build_exit_code(Some(7));

        assert_eq!(result.build_exit_code, Some(7));
    }

    #[test]
    fn test_with_deploy_exit_code() {
        let result = deploy_result().with_deploy_exit_code(Some(8));

        assert_eq!(result.deploy_exit_code, Some(8));
    }

    #[test]
    fn test_with_component_status() {
        let result = deploy_result().with_component_status(ComponentStatus::NeedsUpdate);

        assert!(matches!(
            result.component_status,
            Some(ComponentStatus::NeedsUpdate)
        ));
    }

    #[test]
    fn deployed_version_comparison_distinguishes_direction_and_unknown_versions() {
        let cases = [
            (Some("1.0.1"), Some("1.0.0"), ComponentStatus::NeedsUpdate),
            (Some("1.0.0"), Some("1.0.1"), ComponentStatus::BehindRemote),
            (Some("1.0.0"), Some("1.0.0"), ComponentStatus::UpToDate),
            (
                Some("1.0.0"),
                Some("1.0.0-rc.1"),
                ComponentStatus::NeedsUpdate,
            ),
            (
                Some("1.0.0-rc.1"),
                Some("1.0.0"),
                ComponentStatus::BehindRemote,
            ),
            (
                Some("not-a-version"),
                Some("1.0.0"),
                ComponentStatus::Unknown,
            ),
            (Some("1.0.0"), None, ComponentStatus::Unknown),
            (None, Some("1.0.0"), ComponentStatus::Unknown),
        ];

        for (local, remote, expected) in cases {
            assert_eq!(compare_deployed_versions(local, remote), expected);
        }
    }

    #[test]
    fn only_a_newer_configured_source_requires_deploy() {
        assert!(ComponentStatus::NeedsUpdate.requires_deploy());

        for status in [
            ComponentStatus::UpToDate,
            ComponentStatus::BehindRemote,
            ComponentStatus::BehindUpstream,
            ComponentStatus::SourceStale,
            ComponentStatus::Unknown,
        ] {
            assert!(!status.requires_deploy());
        }
    }

    #[test]
    fn test_with_remote_path() {
        let result =
            deploy_result().with_remote_path("/srv/wp-content/plugins/fixture".to_string());

        assert_eq!(
            result.remote_path.as_deref(),
            Some("/srv/wp-content/plugins/fixture")
        );
    }

    #[test]
    fn new_for_project_reports_effective_remote_path() {
        with_isolated_home(|_| {
            install_wordpress_extension();
            let project = Project {
                id: "site".to_string(),
                path_roots: HashMap::from([(
                    "wp_content".to_string(),
                    "/htdocs/wp-content".to_string(),
                )]),
                ..Project::default()
            };

            let result = ComponentDeployResult::new_for_project(
                &component_with_extension(),
                &project,
                "/srv/site",
            );

            assert_eq!(
                result.remote_path.as_deref(),
                Some("/htdocs/wp-content/plugins/fixture")
            );
        });
    }

    #[test]
    fn test_with_release_state() {
        let result = deploy_result().with_release_state(release_state());

        assert_eq!(
            result.release_state.as_ref().map(ReleaseState::status),
            Some(ReleaseStateStatus::NeedsRelease)
        );
    }

    #[test]
    fn test_with_deployed_ref() {
        let result = deploy_result().with_deployed_ref("v1.2.3".to_string());

        assert_eq!(result.deployed_ref.as_deref(), Some("v1.2.3"));
    }

    #[test]
    fn exact_ref_identity_is_persisted_in_serialized_deploy_evidence() {
        let result = deploy_result().with_exact_ref_identity(
            "origin/reviewed",
            "0123456789abcdef0123456789abcdef01234567",
            "/repos/component",
            "local",
        );
        let evidence = serde_json::to_value(result).expect("serialize deploy evidence");

        assert_eq!(evidence["requested_ref"], "origin/reviewed");
        assert_eq!(
            evidence["resolved_sha"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(evidence["source"], "/repos/component");
        assert_eq!(evidence["resolution_mode"], "local");
        assert_eq!(evidence["deployed_ref"], "origin/reviewed");
        assert_eq!(
            evidence["git_head"],
            "0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn test_with_source_identity_preserves_local_path_for_non_git_component() {
        let component = component();
        let result = deploy_result().with_source_identity(&component, true);

        assert_eq!(result.local_path.as_deref(), Some("/tmp/fixture"));
        assert!(result.git_head.is_none());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_with_source_identity_warns_for_head_deploy_from_linked_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let linked = temp.path().join("linked");
        std::fs::create_dir(&repo).expect("repo dir");
        std::fs::write(repo.join("README.md"), "fixture\n").expect("fixture file");

        run_git(&repo, &["init"]);
        run_git(&repo, &["add", "README.md"]);
        run_git(
            &repo,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        run_git(&repo, &["worktree", "add", linked.to_str().unwrap()]);

        let mut component = component();
        component.local_path = linked.to_string_lossy().to_string();
        let result = deploy_result().with_source_identity(&component, true);

        assert_eq!(result.is_worktree, Some(true));
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("non-primary git worktree")));
    }

    #[test]
    fn test_with_source_identity_warns_for_detached_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).expect("repo dir");
        std::fs::write(repo.join("README.md"), "fixture\n").expect("fixture file");

        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["add", "README.md"]);
        run_git(
            &repo,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        run_git(&repo, &["checkout", "--detach", "HEAD"]);

        let mut component = component();
        component.local_path = repo.to_string_lossy().to_string();
        let result = deploy_result().with_source_identity(&component, false);

        assert!(result.git_branch.is_none());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("configured_source_detached")));
    }

    #[test]
    fn prepared_artifact_rejects_changed_bytes_and_version_before_deploy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("fixture.zip");
        std::fs::write(&artifact_path, "release bytes").expect("artifact");
        let artifact = PreparedDeployArtifact {
            component_id: "fixture".to_string(),
            path: artifact_path.display().to_string(),
            durable_path: artifact_path.display().to_string(),
            size_bytes: 13,
            sha256: super::sha256_file(&artifact_path).expect("sha"),
            version: "1.2.3".to_string(),
            tag: "v1.2.3".to_string(),
            source_commit: "0123456789abcdef".to_string(),
        };

        assert!(artifact.validate("fixture", Some("1.2.3")).is_ok());
        assert!(artifact.validate("fixture", Some("1.2.4")).is_err());
        assert!(artifact
            .validate_exact_source("fixture", Some("1.2.3"), "0123456789abcdef")
            .is_ok());
        assert!(artifact
            .validate_exact_source("fixture", Some("1.2.3"), "fedcba9876543210")
            .is_err());

        std::fs::write(&artifact_path, "changed bytes").expect("changed artifact");
        assert!(artifact.validate("fixture", Some("1.2.3")).is_err());
    }

    #[test]
    fn prepared_artifact_identity_is_identical_in_each_target_result() {
        let artifact = PreparedDeployArtifact {
            component_id: "fixture".to_string(),
            path: "/source/fixture.zip".to_string(),
            durable_path: "/durable/fixture.zip".to_string(),
            size_bytes: 12,
            sha256: "abc123".to_string(),
            version: "1.2.3".to_string(),
            tag: "v1.2.3".to_string(),
            source_commit: "0123456789abcdef".to_string(),
        };
        let first = deploy_result().with_prepared_artifact(artifact.clone());
        let second = deploy_result().with_prepared_artifact(artifact);

        assert_eq!(first.prepared_artifact, second.prepared_artifact);
        assert_eq!(
            serde_json::to_value(&first).expect("first evidence")["prepared_artifact"]["sha256"],
            serde_json::to_value(&second).expect("second evidence")["prepared_artifact"]["sha256"],
        );
    }

    fn run_git(path: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn release_state_status_uses_needs_release_public_name() {
        let state = release_state();

        assert_eq!(state.status(), ReleaseStateStatus::NeedsRelease);
        assert_eq!(state.status().as_str(), "needs_release");
        assert_eq!(
            serde_json::to_value(state.status()).expect("serialize status"),
            serde_json::json!("needs_release")
        );
    }
}

/// Result of deploying to a single project within a multi-project run.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDeployResult {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_timings: Option<PhaseTimingReport>,
}

/// Result of a multi-project deployment.
#[derive(Debug, Clone, Serialize)]
pub struct MultiDeployResult {
    pub component_ids: Vec<String>,
    pub projects: Vec<ProjectDeployResult>,
    pub summary: MultiDeploySummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_run_id: Option<String>,
}

/// Summary of multi-project deployment.
#[derive(Debug, Clone, Serialize)]
pub struct MultiDeploySummary {
    pub total_projects: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
    pub planned: u32,
}

/// Summary of deploy orchestration.
#[derive(Debug, Clone, Serialize)]

pub struct DeploySummary {
    pub total: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

/// Result of deploy orchestration for multiple components.
#[derive(Debug, Clone, Serialize)]

pub struct DeployOrchestrationResult {
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}
