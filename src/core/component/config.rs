use serde::{ser::SerializeMap, Deserialize, Serialize, Serializer};
use std::collections::HashMap;

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
}

impl ComponentReleaseConfig {
    pub fn is_default(value: &Self) -> bool {
        value == &Self::default()
    }
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
