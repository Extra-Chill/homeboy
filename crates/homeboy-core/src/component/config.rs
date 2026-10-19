use serde::{ser::SerializeMap, Deserialize, Serialize, Serializer};
use std::collections::HashMap;

use homeboy_error::{Error, Result as HomeboyResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]

pub struct VersionTarget {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Path to verify inside the deploy artifact (ZIP), when it differs from `file`.
    ///
    /// `file` is bumped in the workspace (git-tracked source), while `artifact_path`
    /// is what the verifier looks for inside the shipped artifact. This is needed for
    /// components that bump source manifests but ship compiled manifests from a
    /// different artifact path. When unset, the verifier falls back to `file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScopedExtensionConfig {
    /// Version constraint string (e.g., ">=2.0.0", "^1.0").
    pub version: Option<String>,
    /// Settings passed to the extension at runtime.
    ///
    /// Populated from `settings` and flat keys that aren't reserved struct fields:
    ///
    /// ```json
    /// { "settings": { "database_type": "mysql" } }
    /// { "database_type": "mysql", "mysql_host": "localhost" }
    /// ```
    pub settings: HashMap<String, serde_json::Value>,
}

impl serde::Serialize for ScopedExtensionConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = self.settings.len() + usize::from(self.version.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        if let Some(version) = self.version.as_ref() {
            map.serialize_entry("version", version)?;
        }
        for (key, value) in &self.settings {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for ScopedExtensionConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize the whole object as a generic JSON map first.
        let mut map: serde_json::Map<String, serde_json::Value> =
            serde::Deserialize::deserialize(deserializer)?;

        // Extract known struct fields.
        let version = map
            .remove("version")
            .and_then(|v| v.as_str().map(String::from));

        let mut settings = map
            .remove("settings")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
            .into_iter()
            .collect::<HashMap<_, _>>();

        // Flat keys are the existing convention and retain precedence when both
        // shapes are present.
        settings.extend(map);

        Ok(ScopedExtensionConfig { version, settings })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandScopeConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScopeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<CommandScopeConfig>,
    /// Package completeness only: which tracked runtime files the release
    /// archive must contain. Unlike `release`, this never affects which commits
    /// count as releasable, so a source tree that ships as a compiled build
    /// output can be excluded here without hiding its changes from releases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<CommandScopeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComponentScriptsConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lint: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bench: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fuzz: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComponentOverrideConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_deploy: Option<GitDeployConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hooks: HashMap<homeboy_extension_contract::HookEvent, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<ScopeConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<ArtifactInput>,
    /// Override the CLI path used by extension deploy install steps.
    /// For example, local wrappers may need "lando wp" instead of the default "wp".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<String>,
}

impl ComponentOverrideConfig {
    /// Apply one explicit override layer to a component.
    ///
    /// `None` and empty collection fields mean "no override" so existing
    /// on-disk config keeps the same sparse-layer behavior.
    pub fn apply_to_component(&self, component: &mut super::model::Component) {
        if let Some(remote_path) = &self.remote_path {
            component.remote_path = remote_path.clone();
        }
        if let Some(build_artifact) = &self.build_artifact {
            component.build_artifact = Some(build_artifact.clone());
        }
        if let Some(extract_command) = &self.extract_command {
            component.extract_command = Some(extract_command.clone());
        }
        if let Some(remote_owner) = &self.remote_owner {
            component.remote_owner = Some(remote_owner.clone());
        }
        if let Some(deploy_strategy) = &self.deploy_strategy {
            component.deploy_strategy = Some(deploy_strategy.clone());
        }
        if let Some(git_deploy) = &self.git_deploy {
            component.git_deploy = Some(git_deploy.clone());
        }
        if !self.hooks.is_empty() {
            component.hooks = self.hooks.clone();
        }
        if let Some(scopes) = &self.scopes {
            component.scopes = Some(scopes.clone());
        }
        if !self.artifact_inputs.is_empty() {
            component.artifact_inputs = self.artifact_inputs.clone();
        }
        if let Some(cli_path) = &self.cli_path {
            component.cli_path = Some(cli_path.clone());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DependencyStackEdge {
    pub upstream: String,
    pub downstream: String,
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub rebuild: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_update: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test: Vec<String>,
    /// Remote release source for this edge's upstream.
    ///
    /// Declaring a source lets a downstream resolve its upstream's newest
    /// release without the upstream being registered locally, using the same
    /// tag resolution as `homeboy release resolve`. Edges without a source
    /// keep resolving through the local component registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DependencyStackEdgeSource>,
}

/// Remote release source declared on a [`DependencyStackEdge`].
///
/// `repo` is a remote Git repository and `prefix` a namespaced tag namespace:
/// the newest `refs/tags/<prefix>-v<semver>` tag names the upstream's newest
/// release.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DependencyStackEdgeSource {
    pub repo: String,
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ArtifactInput {
    pub component: String,
    pub artifact: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// A generated artifact that must exist before a component can be packaged.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PackageArtifact {
    /// File or glob relative to the component source root.
    pub path: String,
    /// Command that reconstructs the artifact from the selected source tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconstruct_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GithubConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hosts: HashMap<String, GithubHostConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GithubHostConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ComponentReleaseConfig {
    /// Who owns cutting releases for this component.
    ///
    /// Absent (or `local`) preserves the default behaviour: a local
    /// `homeboy release` plans, gates, tags, and publishes. `ci` declares that
    /// the component's CI (the reusable homeboy-action release workflow)
    /// computes and publishes every release, so a local `homeboy release`
    /// reports the CI ownership and performs no mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<ReleaseOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_release: Option<ComponentGithubReleaseConfig>,
    /// Whether extension-provided registry or package publication is permitted.
    ///
    /// Absent preserves the extension capability's default behavior. Set this to
    /// `false` when release assets are the component's deliverable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<bool>,
    /// Allow contributors to edit `changelog_target` outside Homeboy releases.
    /// Disabled by default because configured changelogs are release-generated.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_manual_changelog_edits: bool,
    /// Configured version-target paths that intentionally receive manual
    /// maintenance outside a Homeboy release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_version_targets: Vec<String>,
    /// Extension-declared release lockfiles intentionally maintained manually.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manual_release_lockfiles: Vec<String>,
    /// Per-ZIP source-to-archive coverage declarations for transformed packages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_coverage: Vec<PackageCoverageConfig>,
    /// Git subtree split publications performed as part of a release.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtree: Vec<SubtreePublicationConfig>,
    /// Release gates run against the packaged artifact before tagging.
    ///
    /// Gates run after packaging, in declaration order, fail-fast. A non-zero
    /// exit blocks the release before any tag or publication.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<ReleaseGateConfig>,
}

/// One declared release gate (`release.gates[]`).
///
/// A gate is a command run from the component root against the exact artifact
/// the release will publish. `{artifact}` in `run` is replaced with the
/// shell-quoted path of the selected artifact.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ReleaseGateConfig {
    /// Unique gate identifier.
    pub id: String,
    /// Command run from the component root after packaging. `{artifact}` is
    /// replaced with the shell-quoted path of the built artifact.
    pub run: String,
    /// Selects which built artifact the gate receives. Absent selects the
    /// release's primary artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ReleaseGateArtifactSelector>,
    /// Component-relative directory the gate writes; Homeboy attaches it to
    /// the release run's durable artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Artifact selector for a [`ReleaseGateConfig`].
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ReleaseGateArtifactSelector {
    /// Packaging profile whose artifact the gate receives, matched against the
    /// built artifact file names. Must select exactly one artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Exact component-relative artifact path to hand the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubtreePublicationConfig {
    /// Repository-relative source directory to split.
    pub prefix: String,
    /// Destination repository URL or configured remote name.
    pub remote: String,
    /// Destination branch, without the `refs/heads/` prefix.
    pub branch: String,
    /// Publish the exact release tag to the destination repository.
    #[serde(default = "default_true")]
    pub tag: bool,
    /// Destination tag template. `{version}` is replaced with the release version.
    /// Defaults to `v{version}` when tag publication is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_template: Option<String>,
    /// How an existing destination branch may change.
    #[serde(default)]
    pub branch_policy: SubtreeBranchPolicy,
    /// Maximum wall-clock time for rewriting the source history during a split.
    /// Defaults to five minutes; set this when the source history needs longer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_timeout_secs: Option<u64>,
}

impl Default for SubtreePublicationConfig {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            remote: String::new(),
            branch: String::new(),
            tag: true,
            tag_template: None,
            branch_policy: SubtreeBranchPolicy::default(),
            split_timeout_secs: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubtreeBranchPolicy {
    /// Update only when the split commit is a fast-forward of the destination.
    #[default]
    FastForward,
    /// Never update the destination branch; publish the release tag only.
    TagOnly,
}

fn default_true() -> bool {
    true
}

impl ComponentReleaseConfig {
    pub fn is_default(value: &Self) -> bool {
        value == &Self::default()
    }

    pub fn publish_enabled(&self) -> bool {
        self.publish.unwrap_or(true)
    }

    /// Effective release owner; absent means [`ReleaseOwner::Local`].
    pub fn release_owner(&self) -> ReleaseOwner {
        self.owner.unwrap_or_default()
    }

    /// Whether the component declares CI as the owner of its releases.
    pub fn ci_owns_release(&self) -> bool {
        self.release_owner() == ReleaseOwner::Ci
    }

    /// Validate declared release gates: every gate names a unique, non-empty
    /// `id` and a non-empty `run` command, and any evidence directory is a
    /// safe relative path.
    pub fn validate_gates(&self) -> HomeboyResult<()> {
        let mut ids: Vec<&str> = Vec::new();
        for gate in &self.gates {
            let id = gate.id.trim();
            if id.is_empty() {
                return Err(release_gate_error("every release gate requires an id"));
            }
            if ids.contains(&id) {
                return Err(release_gate_error(&format!(
                    "release gate ids must be unique; '{}' is declared more than once",
                    id
                )));
            }
            ids.push(id);
            if gate.run.trim().is_empty() {
                return Err(release_gate_error(&format!(
                    "release gate '{}' requires a non-empty run command",
                    id
                )));
            }
            if let Some(selector) = gate.artifact.as_ref() {
                let has_profile = selector
                    .profile
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                let has_path = selector
                    .path
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                if !has_profile && !has_path {
                    return Err(release_gate_error(&format!(
                        "release gate '{}' declares an artifact selector without a profile or path",
                        id
                    )));
                }
            }
            if let Some(evidence) = gate.evidence.as_deref() {
                validate_canonical_coverage_path(evidence, "evidence")
                    .map_err(|err| release_gate_error(&err.message))?;
            }
        }
        Ok(())
    }

    pub fn validate_package_coverage(&self) -> HomeboyResult<()> {
        let mut selectors = Vec::new();
        for coverage in &self.package_coverage {
            validate_canonical_coverage_path(&coverage.artifact, "artifact")?;
            validate_canonical_coverage_path(&coverage.archive_root, "archive_root")?;
            let selector = (coverage.artifact_match, &coverage.artifact);
            if selectors.contains(&selector) {
                return Err(package_coverage_error(
                    "artifact selectors must be unique across package coverage declarations",
                ));
            }
            selectors.push(selector);
            if coverage.source_roots.is_empty() {
                return Err(package_coverage_error(
                    "source_roots must contain at least one repository-relative directory",
                ));
            }

            let mut roots = Vec::new();
            for root in &coverage.source_roots {
                validate_canonical_coverage_path(root, "source_roots")?;
                if roots.iter().any(|existing: &String| {
                    root == "."
                        || existing == "."
                        || root == existing
                        || root
                            .strip_prefix(existing)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                        || existing
                            .strip_prefix(root)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                }) {
                    return Err(package_coverage_error(
                        "source_roots must not overlap within one package coverage declaration",
                    ));
                }
                roots.push(root.clone());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PackageCoverageConfig {
    /// Artifact path or glob selector for one emitted ZIP artifact.
    pub artifact: String,
    /// Whether `artifact` is an exact path (the default) or a glob pattern.
    pub artifact_match: PackageCoverageArtifactMatch,
    /// Repository-relative source directories whose runtime files must be covered.
    pub source_roots: Vec<String>,
    /// Archive-relative directory containing the mapped source-root contents.
    pub archive_root: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackageCoverageArtifactMatch {
    #[default]
    Exact,
    Glob,
}

#[derive(Deserialize)]
struct RawPackageCoverageConfig {
    artifact: String,
    #[serde(default)]
    artifact_match: PackageCoverageArtifactMatch,
    source_roots: Vec<String>,
    archive_root: String,
}

impl<'de> Deserialize<'de> for PackageCoverageConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPackageCoverageConfig::deserialize(deserializer)?;
        let artifact =
            normalize_coverage_path(&raw.artifact, "artifact").map_err(serde::de::Error::custom)?;
        let source_roots = raw
            .source_roots
            .iter()
            .map(|root| normalize_coverage_path(root, "source_roots"))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(serde::de::Error::custom)?;
        let archive_root = normalize_coverage_path(&raw.archive_root, "archive_root")
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            artifact,
            artifact_match: raw.artifact_match,
            source_roots,
            archive_root,
        })
    }
}

fn normalize_coverage_path(value: &str, field: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
    {
        return Err(format!("{} must be a non-empty relative path", field));
    }

    let normalized = value.replace('\\', "/");
    let mut segments = Vec::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => continue,
            ".." => return Err(format!("{} must not contain parent traversal", field)),
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        Ok(".".to_string())
    } else {
        Ok(segments.join("/"))
    }
}

fn validate_canonical_coverage_path(value: &str, field: &str) -> HomeboyResult<()> {
    let normalized = normalize_coverage_path(value, field)
        .map_err(|message| package_coverage_error(&message))?;
    if value != normalized {
        return Err(package_coverage_error(&format!(
            "{} must use canonical slash-separated form '{}'",
            field, normalized
        )));
    }
    Ok(())
}

fn release_gate_error(message: &str) -> Error {
    Error::validation_invalid_argument(
        "release.gates",
        message,
        None,
        Some(vec![
            "Declare each gate with a unique id and a run command; optional artifact selector and evidence directory.".to_string(),
        ]),
    )
}

fn package_coverage_error(message: &str) -> Error {
    Error::validation_invalid_argument(
        "release.package_coverage",
        message,
        None,
        Some(vec![
            "Declare one artifact selector, one or more non-overlapping source_roots, and an archive_root.".to_string(),
        ]),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ComponentGithubReleaseConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<GithubReleaseOwner>,
}

/// Owner of a component's release lifecycle (`release.owner`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseOwner {
    /// A local `homeboy release` (the controller) cuts releases. Default.
    #[default]
    Local,
    /// The component's CI release workflow cuts releases; local runs defer.
    Ci,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubReleaseOwner {
    Homeboy,
    Ci,
}

pub(super) fn is_default_github_config(config: &GithubConfig) -> bool {
    config.hosts.is_empty()
}

#[derive(Debug, Clone, Copy)]
pub struct ComponentDeployConfig<'a> {
    pub local_path: &'a str,
    pub remote_path: &'a str,
    pub build_artifact: Option<&'a str>,
    pub extract_command: Option<&'a str>,
    pub remote_owner: Option<&'a str>,
    pub deploy_strategy: Option<&'a str>,
    pub git_deploy: Option<&'a GitDeployConfig>,
    pub remote_url: Option<&'a str>,
    pub cli_path: Option<&'a str>,
    pub artifact_inputs: &'a [ArtifactInput],
    pub cleanup_artifacts: &'a [CleanupArtifactDeclaration],
}

/// Repository-owned deployment policy executed by an extension-declared provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentProviderAttachment {
    pub extension: String,
    pub provider: String,
    /// Legacy repository-relative contract file for unlayered providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    /// Opaque inline repository policy for layered providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<serde_json::Value>,
}

impl ComponentDeployConfig<'_> {
    pub fn is_git_deploy(&self) -> bool {
        self.deploy_strategy == Some("git")
    }

    pub fn is_file_deploy(&self) -> bool {
        self.deploy_strategy == Some("file")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupArtifactDeclaration {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitDeployConfig {
    /// Git remote to pull from (default: "origin")
    #[serde(
        default = "default_git_remote",
        skip_serializing_if = "is_default_remote"
    )]
    pub remote: String,
    /// Branch to pull (default: "main")
    #[serde(
        default = "default_git_branch",
        skip_serializing_if = "is_default_branch"
    )]
    pub branch: String,
    /// Commands to run after git pull.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_pull: Vec<String>,
    /// Pull a specific tag instead of branch HEAD (e.g., "v{{version}}")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<String>,
}

fn default_git_remote() -> String {
    "origin".to_string()
}
fn default_git_branch() -> String {
    "main".to_string()
}
fn is_default_remote(s: &str) -> bool {
    s == "origin"
}
fn is_default_branch(s: &str) -> bool {
    s == "main"
}
pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::ComponentReleaseConfig;

    #[test]
    fn manual_changelog_edit_policy_is_absent_when_defaulted() {
        let value = serde_json::to_value(ComponentReleaseConfig::default()).expect("serialize");

        assert!(value.get("publish").is_none());
        assert!(value.get("allow_manual_changelog_edits").is_none());
        assert!(value.get("manual_version_targets").is_none());
        assert!(value.get("manual_release_lockfiles").is_none());
    }

    #[test]
    fn release_gates_are_absent_when_defaulted_and_round_trip_when_declared() {
        let value = serde_json::to_value(ComponentReleaseConfig::default()).expect("serialize");
        assert!(value.get("gates").is_none());

        let config: ComponentReleaseConfig = serde_json::from_value(serde_json::json!({
            "gates": [{
                "id": "html-site-import",
                "artifact": { "profile": "html-site-import" },
                "run": "node tools/run-release-import-gate.mjs --zip {artifact}",
                "evidence": "release-import-gate-evidence"
            }]
        }))
        .expect("release gates configuration");

        assert_eq!(config.gates.len(), 1);
        assert_eq!(config.gates[0].id, "html-site-import");
        assert_eq!(
            config.gates[0]
                .artifact
                .as_ref()
                .and_then(|selector| selector.profile.as_deref()),
            Some("html-site-import")
        );
        config.validate_gates().expect("valid gates");
    }

    #[test]
    fn release_gate_validation_requires_unique_ids_and_run_commands() {
        let duplicate: ComponentReleaseConfig = serde_json::from_value(serde_json::json!({
            "gates": [
                { "id": "import", "run": "true" },
                { "id": "import", "run": "true" }
            ]
        }))
        .expect("configuration");
        assert!(duplicate.validate_gates().is_err());

        let empty_run: ComponentReleaseConfig = serde_json::from_value(serde_json::json!({
            "gates": [{ "id": "import", "run": "  " }]
        }))
        .expect("configuration");
        assert!(empty_run.validate_gates().is_err());

        let empty_id: ComponentReleaseConfig = serde_json::from_value(serde_json::json!({
            "gates": [{ "id": "", "run": "true" }]
        }))
        .expect("configuration");
        assert!(empty_id.validate_gates().is_err());

        let empty_selector: ComponentReleaseConfig = serde_json::from_value(serde_json::json!({
            "gates": [{ "id": "import", "run": "true", "artifact": {} }]
        }))
        .expect("configuration");
        assert!(empty_selector.validate_gates().is_err());

        let traversal_evidence: ComponentReleaseConfig =
            serde_json::from_value(serde_json::json!({
                "gates": [{ "id": "import", "run": "true", "evidence": "../evidence" }]
            }))
            .expect("configuration");
        assert!(traversal_evidence.validate_gates().is_err());
    }

    #[test]
    fn release_publish_defaults_to_enabled_and_accepts_an_explicit_opt_out() {
        let disabled: ComponentReleaseConfig =
            serde_json::from_value(serde_json::json!({ "publish": false }))
                .expect("release configuration");

        assert!(ComponentReleaseConfig::default().publish_enabled());
        assert!(!disabled.publish_enabled());
    }
}
