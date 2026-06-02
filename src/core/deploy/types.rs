use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::core::artifact_inputs::ResolvedArtifactInput;
use crate::core::component::Component;
use crate::core::config;
use crate::core::error::Result;
use crate::core::paths as base_path;
use crate::core::project::Project;
use crate::is_zero_u32;

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
}

impl DeployResult {
    pub(super) fn success(exit_code: i32) -> Self {
        Self {
            success: true,
            exit_code,
            error: None,
        }
    }

    pub(super) fn failure(exit_code: i32, error: String) -> Self {
        Self {
            success: false,
            exit_code,
            error: Some(error),
        }
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
    /// Assert expected version before deploying (abort if mismatch)
    pub expected_version: Option<String>,
    /// Skip auto-pulling latest changes before deploy
    pub no_pull: bool,
    /// Deploy from current branch HEAD instead of latest tag
    pub head: bool,
    /// Force tag-based deploy, ignoring any reusable build artifacts
    pub tagged: bool,
}

/// Reason why a component was selected for deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployReason {
    /// Component was explicitly specified by ID
    ExplicitlySelected,
    /// --all flag was used
    AllSelected,
    /// Local and remote versions differ
    VersionMismatch,
    /// Could not determine local version
    UnknownLocalVersion,
    /// Could not determine remote version (not deployed or no version file)
    UnknownRemoteVersion,
}

/// Status indicator for component version comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    /// Local and remote versions match
    UpToDate,
    /// Local version ahead of remote (needs deploy)
    NeedsUpdate,
    /// Remote version ahead of local (local behind)
    BehindRemote,
    /// Local checkout is behind its upstream branch
    BehindUpstream,
    /// Remote matches a configured source checkout that is stale or detached
    SourceStale,
    /// Cannot determine status
    Unknown,
}

/// Release state tracking for deployment decisions.
/// Captures git state relative to the last version tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseState {
    /// Number of commits since the last version tag
    pub commits_since_version: u32,
    /// Number of code commits (non-docs)
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub code_commits: u32,
    /// Number of docs-only commits
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub docs_only_commits: u32,
    /// Whether there are uncommitted changes in the working directory
    pub has_uncommitted_changes: bool,
    /// The baseline reference (tag or commit hash) used for comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
    /// Warning emitted when the detected baseline may not align with the current version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_warning: Option<String>,
}

/// High-level status derived from a component release state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStateStatus {
    Uncommitted,
    NeedsRelease,
    DocsOnly,
    Clean,
    Unknown,
}

impl ReleaseState {
    pub fn status(&self) -> ReleaseStateStatus {
        if self.has_uncommitted_changes {
            ReleaseStateStatus::Uncommitted
        } else if self.code_commits > 0 {
            ReleaseStateStatus::NeedsRelease
        } else if self.docs_only_commits > 0 {
            ReleaseStateStatus::DocsOnly
        } else {
            ReleaseStateStatus::Clean
        }
    }
}

impl ReleaseStateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseStateStatus::Uncommitted => "uncommitted",
            ReleaseStateStatus::NeedsRelease => "needs_release",
            ReleaseStateStatus::DocsOnly => "docs_only",
            ReleaseStateStatus::Clean => "clean",
            ReleaseStateStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseStateBuckets {
    pub ready_to_deploy: Vec<String>,
    pub needs_release: Vec<String>,
    pub docs_only: Vec<String>,
    pub has_uncommitted: Vec<String>,
    pub unknown: Vec<String>,
}

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
            artifact_inputs: Vec::new(),
            remote_path: base_path::join_remote_path(Some(base_path), &component.remote_path).ok(),
            build_exit_code: None,
            deploy_exit_code: None,
            release_state: None,
            deployed_ref: None,
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

    pub(super) fn with_source_identity(mut self, component: &Component, head_deploy: bool) -> Self {
        let path = Path::new(&component.local_path);
        self.local_path = Some(component.local_path.clone());
        self.git_branch = git_output(path, &["branch", "--show-current"]);
        self.git_head = git_output(path, &["rev-parse", "HEAD"]);
        self.upstream_branch = git_output(path, &["rev-parse", "--abbrev-ref", "@{upstream}"])
            .or_else(|| default_remote_branch(path));
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

fn default_remote_branch(path: &Path) -> Option<String> {
    git_output(
        path,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .or_else(|| {
        ["origin/main", "origin/trunk", "origin/master"]
            .iter()
            .find(|branch| git_output(path, &["rev-parse", "--verify", branch]).is_some())
            .map(|branch| (*branch).to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        parse_bulk_component_ids, ComponentDeployResult, ComponentStatus, DeployResult,
        ReleaseState, ReleaseStateStatus,
    };
    use crate::core::component::{Component, ScopedExtensionConfig};
    use crate::core::extension::{DeployCapability, ExtensionManifest, RemotePathRootRule};
    use crate::core::project::Project;
    use crate::test_support::with_isolated_home;
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
        crate::core::extension::save_manifest(&ExtensionManifest {
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
}

/// Result of a multi-project deployment.
#[derive(Debug, Clone, Serialize)]
pub struct MultiDeployResult {
    pub component_ids: Vec<String>,
    pub projects: Vec<ProjectDeployResult>,
    pub summary: MultiDeploySummary,
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
