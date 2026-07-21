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
    pub audit: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refactor: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<CommandScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<CommandScopeConfig>,
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
    pub hooks: HashMap<String, Vec<String>>,
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
    pub fn apply_to_component(&self, component: &mut crate::model::Component) {
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ArtifactInput {
    pub component: String,
    pub artifact: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_release: Option<ComponentGithubReleaseConfig>,
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
}

impl ComponentReleaseConfig {
    pub fn is_default(value: &Self) -> bool {
        value == &Self::default()
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

        assert!(value.get("allow_manual_changelog_edits").is_none());
        assert!(value.get("manual_version_targets").is_none());
        assert!(value.get("manual_release_lockfiles").is_none());
    }
}
