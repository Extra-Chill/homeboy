use serde::{ser::SerializeMap, Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{HashMap, HashSet};

pub mod artifacts;
pub mod audit;
pub mod drift;
pub mod inventory;
pub mod mutations;
pub mod portable;
pub mod relationships;
pub mod resolution;
pub mod scope;
pub mod versioning;

pub use artifacts::{cleanup_artifact_report, CleanupArtifactCandidate, CleanupArtifactReport};
pub use audit::{
    ArtifactPortabilityConfig, AuditConfig, CommandStatusContractConfig,
    CommandStatusContractScenario, ConfigKeyUsageConfig, ConfigKeyUsagePattern, ConfigKeyUsageRule,
    ConventionTagGlob, CoreBoundaryLeakConfig, DuplicationDetectorConfig, KnownSymbolEntry,
    KnownSymbolHeaderVersionProvider, KnownSymbolKind, KnownSymbolVersionedEntry,
    MutatingResourceAccessConfig, PublicRegistryExposureConfig, RedirectValidationConfig,
    RequestedDetectorRule, RequestedDetectorRuleBody, RequiredRegexScope,
};
pub use inventory::{
    exists, extension_provides_artifact_pattern, inventory, list, list_ids, load,
    reconcile_standalone_registration, write_standalone_component_config,
    write_standalone_registration, ComponentReconcileReport,
};
pub use mutations::{delete_safe, merge, rename};
pub use portable::{
    discover_from_portable, infer_portable_component_id, mutate_portable, portable_json,
    try_discover_from_portable, write_portable_config,
};
pub use relationships::{associated_projects, projects_using, rename_component, shared_components};
pub use resolution::{
    resolve, resolve_artifact, resolve_effective, resolve_target, resolve_target_from_component,
    validate_local_path, ResolvedTarget, TargetSpec,
};
pub use scope::{resolve_component_scope, EffectiveScope, ScopeCommand};
pub use versioning::{
    normalize_version_pattern, parse_version_targets, validate_version_pattern,
    validate_version_target_conflict,
};

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VersionTarget {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
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
pub struct ComponentLabConfig {
    /// Repo-owned argv prefix used when remote execution re-enters this checkout.
    ///
    /// By default, remote execution uses the runner's configured command binary.
    /// Repos that need to verify the synced checkout itself can declare an argv
    /// prefix such as `["tool", "run", "--"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub self_command_prefix: Vec<String>,
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
#[serde(from = "RawComponent", into = "RawComponent")]
pub struct Component {
    pub id: String,
    pub aliases: Vec<String>,
    pub local_path: String,
    pub remote_path: String,
    pub build_artifact: Option<String>,
    pub build_command: Option<String>,
    pub extensions: Option<HashMap<String, ScopedExtensionConfig>>,
    pub version_targets: Option<Vec<VersionTarget>>,
    pub changelog_target: Option<String>,
    pub changelog_next_section_label: Option<String>,
    pub changelog_next_section_aliases: Option<Vec<String>>,
    /// Lifecycle hooks: event name -> list of shell commands.
    /// Events: `pre:version:bump`, `post:version:bump`, `post:release`, `post:deploy`
    pub hooks: HashMap<String, Vec<String>>,
    pub extract_command: Option<String>,
    pub remote_owner: Option<String>,
    pub deploy_strategy: Option<String>,
    pub git_deploy: Option<GitDeployConfig>,
    /// Git remote URL for the component's source repository (e.g., GitHub URL).
    /// Used by deploy to download release artifacts or initialize server-side git repos.
    pub remote_url: Option<String>,
    /// Reporting-only GitHub remote override for `homeboy triage`.
    /// Does not affect git, deploy, or release operations.
    pub triage_remote_url: Option<String>,
    /// Labels treated as priority issues by `homeboy triage` for this component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority_labels: Option<Vec<String>>,
    pub auto_cleanup: bool,
    pub docs_dir: Option<String>,
    pub docs_dirs: Vec<String>,
    pub scopes: Option<ScopeConfig>,
    /// Component-owned shell scripts that satisfy Homeboy command capabilities
    /// without requiring an extension. Resolution order is `scripts.*` first,
    /// then extension-claimed behavior, then not-applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<ComponentScriptsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditConfig>,
    /// Component-owned remote runner behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lab: Option<ComponentLabConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_stack: Vec<DependencyStackEdge>,
    /// Override the CLI path used by extension deploy install steps.
    /// For example, Studio sites need "studio wp" instead of the default "wp".
    pub cli_path: Option<String>,
    /// Component-level additions to the merge-aftermath drift list.
    ///
    /// Extensions declare baseline drift (lockfiles) via their manifests.
    /// This field lets a component add extra paths — for example, a
    /// generated SDK file regenerated on every release. Component entries
    /// are additive only; they cannot remove extension-declared drift.
    ///
    /// Paths are repo-root-relative. The audit baseline (`homeboy.json`)
    /// is always drift and does not need to be listed here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_drift_files: Vec<String>,
    /// Reconstructable repo-relative runtime, dependency, or generated outputs
    /// that may be safely removed by cleanup/reporting surfaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_artifacts: Vec<CleanupArtifactDeclaration>,
    /// Subsystem-owned baseline snapshots stored in repo-local portable config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baselines: Option<HashMap<String, serde_json::Value>>,
    /// Subsystem-owned architecture audit rule config stored in repo-local portable config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_rules: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RawComponent {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    #[serde(default)]
    local_path: String,
    #[serde(default)]
    remote_path: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        default,
        deserialize_with = "deserialize_empty_as_none"
    )]
    build_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Option<HashMap<String, ScopedExtensionConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_targets: Option<Vec<VersionTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_next_section_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_next_section_aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    hooks: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extract_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_deploy: Option<GitDeployConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    triage_remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority_labels: Option<Vec<String>>,
    #[serde(default)]
    auto_cleanup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    docs_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    docs_dirs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scopes: Option<ScopeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scripts: Option<ComponentScriptsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audit: Option<AuditConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lab: Option<ComponentLabConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dependency_stack: Vec<DependencyStackEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cli_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_drift_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cleanup_artifacts: Vec<CleanupArtifactDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    baselines: Option<HashMap<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audit_rules: Option<serde_json::Value>,
}

impl From<RawComponent> for Component {
    fn from(raw: RawComponent) -> Self {
        Component {
            id: raw.id,
            aliases: raw.aliases,
            local_path: raw.local_path,
            remote_path: raw.remote_path,
            build_artifact: raw.build_artifact,
            build_command: raw.build_command,
            extensions: raw.extensions,
            version_targets: raw.version_targets,
            changelog_target: raw.changelog_target,
            changelog_next_section_label: raw.changelog_next_section_label,
            changelog_next_section_aliases: raw.changelog_next_section_aliases,
            hooks: raw.hooks,
            extract_command: raw.extract_command,
            remote_owner: raw.remote_owner,
            deploy_strategy: raw.deploy_strategy,
            git_deploy: raw.git_deploy,
            remote_url: raw.remote_url,
            triage_remote_url: raw.triage_remote_url,
            priority_labels: raw.priority_labels,
            auto_cleanup: raw.auto_cleanup,
            docs_dir: raw.docs_dir,
            docs_dirs: raw.docs_dirs,
            scopes: raw.scopes,
            scripts: raw.scripts,
            audit: raw.audit,
            lab: raw.lab,
            dependency_stack: raw.dependency_stack,
            cli_path: raw.cli_path,
            extra_drift_files: raw.extra_drift_files,
            cleanup_artifacts: raw.cleanup_artifacts,
            baselines: raw.baselines,
            audit_rules: raw.audit_rules,
        }
    }
}

impl From<Component> for RawComponent {
    fn from(c: Component) -> Self {
        RawComponent {
            id: c.id,
            aliases: c.aliases,
            local_path: c.local_path,
            remote_path: c.remote_path,
            build_artifact: c.build_artifact,
            build_command: c.build_command,
            extensions: c.extensions,
            version_targets: c.version_targets,
            changelog_target: c.changelog_target,
            changelog_next_section_label: c.changelog_next_section_label,
            changelog_next_section_aliases: c.changelog_next_section_aliases,
            hooks: c.hooks,
            extract_command: c.extract_command,
            remote_owner: c.remote_owner,
            deploy_strategy: c.deploy_strategy,
            git_deploy: c.git_deploy,
            remote_url: c.remote_url,
            triage_remote_url: c.triage_remote_url,
            priority_labels: c.priority_labels,
            auto_cleanup: c.auto_cleanup,
            docs_dir: c.docs_dir,
            docs_dirs: c.docs_dirs,
            scopes: c.scopes,
            scripts: c.scripts,
            audit: c.audit,
            lab: c.lab,
            dependency_stack: c.dependency_stack,
            cli_path: c.cli_path,
            extra_drift_files: c.extra_drift_files,
            cleanup_artifacts: c.cleanup_artifacts,
            baselines: c.baselines,
            audit_rules: c.audit_rules,
        }
    }
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
    /// Commands to run after git pull (e.g., "composer install", "npm run build")
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
fn is_false(value: &bool) -> bool {
    !*value
}

impl Component {
    pub fn new(
        id: String,
        local_path: String,
        remote_path: String,
        build_artifact: Option<String>,
    ) -> Self {
        Self {
            id,
            aliases: Vec::new(),
            local_path,
            remote_path,
            build_artifact,
            build_command: None,
            extensions: None,
            version_targets: None,
            changelog_target: None,
            changelog_next_section_label: None,
            changelog_next_section_aliases: None,
            hooks: HashMap::new(),
            extract_command: None,
            remote_owner: None,
            deploy_strategy: None,
            git_deploy: None,
            remote_url: None,
            triage_remote_url: None,
            priority_labels: None,
            auto_cleanup: false,
            docs_dir: None,
            docs_dirs: Vec::new(),
            scopes: None,
            scripts: None,
            audit: None,
            lab: None,
            dependency_stack: Vec::new(),
            cli_path: None,
            extra_drift_files: Vec::new(),
            cleanup_artifacts: Vec::new(),
            baselines: None,
            audit_rules: None,
        }
    }

    /// Auto-resolve `remote_path` from linked extension deploy rules when not explicitly set.
    ///
    /// Extensions can declare generic file-content checks and target-path templates.
    /// Core does not know framework-specific deploy paths; it only evaluates the
    /// extension-provided contract.
    ///
    /// Extension templates can use the **local directory name** (basename of
    /// `local_path`) separately from the component ID. This keeps deploy paths
    /// correct when a component ID differs from the on-disk package directory.
    ///
    /// Returns `Some(path)` if auto-resolved, `None` if not applicable or not detectable.
    pub fn auto_resolve_remote_path(&self) -> Option<String> {
        // File components cannot auto-resolve — they must have explicit remote_path.
        if std::path::Path::new(&self.local_path).is_file() {
            return None;
        }

        let local = std::path::Path::new(&self.local_path);

        // Use the directory basename as the remote directory name.
        let dir_name = local.file_name()?.to_str()?;

        let mut matches = HashSet::new();
        for extension_id in self.extensions.as_ref()?.keys() {
            let Ok(extension) = crate::core::extension::load_extension(extension_id) else {
                continue;
            };

            for rule in extension.remote_path_inference_rules() {
                if self.remote_path_inference_rule_matches(rule, local, dir_name) {
                    matches.insert(render_remote_path_template(
                        &rule.remote_path,
                        &self.id,
                        dir_name,
                    ));
                }
            }
        }

        if matches.len() == 1 {
            matches.into_iter().next()
        } else {
            None
        }
    }

    fn remote_path_inference_rule_matches(
        &self,
        rule: &crate::core::extension::RemotePathInferenceRule,
        local: &std::path::Path,
        dir_name: &str,
    ) -> bool {
        let relative_file =
            render_remote_path_template(&rule.when_file_contains.file, &self.id, dir_name);
        let relative_path = std::path::Path::new(&relative_file);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return false;
        }

        let file = local.join(relative_path);
        let Ok(content) = std::fs::read_to_string(file) else {
            return false;
        };

        content.contains(&rule.when_file_contains.text)
    }

    /// Check if this component's local_path points to a file (not a directory).
    ///
    /// File components use `deploy_strategy: "file"` and are deployed via
    /// atomic SCP instead of rsync. They skip build, git sync, and tag checkout.
    pub fn is_file_component(&self) -> bool {
        self.deploy_strategy.as_deref() == Some("file")
            || (std::path::Path::new(&self.local_path).is_file() && self.deploy_strategy.is_none())
    }

    pub fn script_commands(
        &self,
        capability: crate::core::extension::ExtensionCapability,
    ) -> &[String] {
        if let Some(scripts) = self.scripts.as_ref() {
            let commands = match capability {
                crate::core::extension::ExtensionCapability::Lint => &scripts.lint,
                crate::core::extension::ExtensionCapability::Test => &scripts.test,
                crate::core::extension::ExtensionCapability::Build => &scripts.build,
                crate::core::extension::ExtensionCapability::Bench => &scripts.bench,
                crate::core::extension::ExtensionCapability::Trace => &scripts.trace,
                crate::core::extension::ExtensionCapability::Deps => &scripts.deps,
            };
            if !commands.is_empty() {
                return commands;
            }
        }

        &[]
    }

    pub fn has_script(&self, capability: crate::core::extension::ExtensionCapability) -> bool {
        !self.script_commands(capability).is_empty()
    }

    pub fn validate_supported_build_config(&self) -> crate::core::Result<()> {
        let Some(command) = self
            .build_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return Ok(());
        };

        Err(crate::core::Error::validation_invalid_argument(
            "build_command",
            format!(
                "Component '{}' uses unsupported legacy build_command. Use scripts.build instead.",
                self.id
            ),
            Some(command.to_string()),
            Some(vec![
                "Move the command into homeboy.json as: \"scripts\": { \"build\": [\"<command>\"] }".to_string(),
                "Homeboy does not execute component-level build_command.".to_string(),
            ]),
        ))
    }

    /// Ensure `remote_path` is populated. If empty, attempt auto-resolution.
    ///
    /// This should be called after all config layers (repo portable, project overrides)
    /// have been applied. It fills in `remote_path` only if still empty.
    pub fn resolve_remote_path(&mut self) {
        if self.remote_path.trim().is_empty() {
            if let Some(resolved) = self.auto_resolve_remote_path() {
                self.remote_path = resolved;
            }
        }
    }
}

fn render_remote_path_template(template: &str, component_id: &str, dir_name: &str) -> String {
    template
        .replace("{{component_id}}", component_id)
        .replace("{{id}}", component_id)
        .replace("{{dir_name}}", dir_name)
}

/// Normalize empty strings to None. Treats "", null, and field omission identically for consistent validation.
fn deserialize_empty_as_none<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

// ============================================================================
// Runtime resolution + repo-backed component access
// ============================================================================

// ============================================================================
// Operations
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn with_isolated_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        crate::test_support::with_isolated_home(|home| f(home.path()))
    }

    fn write_extension_fixture(home: &Path, id: &str, deploy_json: &str) {
        let dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&dir).expect("extension dir");
        std::fs::write(
            dir.join(format!("{}.json", id)),
            format!(
                r#"{{
  "name": "{} extension",
  "version": "1.0.0",
  "deploy": {}
}}"#,
                id, deploy_json
            ),
        )
        .expect("extension manifest");
    }

    #[test]
    fn validate_version_target_conflict_different_pattern_errors() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result = validate_version_target_conflict(
            &existing,
            "plugin.php",
            "define('VER', '(.*)')",
            "test-comp",
        );
        // Multiple targets per file with different patterns are now allowed
        // (e.g. plugin header Version: + PHP define() constant in same file)
        assert!(result.is_ok());
    }

    #[test]
    fn component_priority_labels_serialization_roundtrip() {
        let mut component = Component::new(
            "data-machine".to_string(),
            "/tmp/data-machine".to_string(),
            "wp-content/plugins/data-machine".to_string(),
            None,
        );
        component.priority_labels = Some(vec!["urgent".to_string()]);

        let json = serde_json::to_string(&component).unwrap();
        let parsed: Component = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.priority_labels, Some(vec!["urgent".to_string()]));
    }

    #[test]
    fn component_ignores_legacy_hook_fields() {
        let component: Component = serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "pre_version_bump_commands": ["cargo build"],
            "post_version_bump_commands": ["cargo test"],
            "post_release_commands": ["echo done"],
            "hooks": {
                "post:deploy": ["wp cache flush"]
            }
        }))
        .unwrap();

        assert!(component.hooks.get("pre:version:bump").is_none());
        assert!(component.hooks.get("post:version:bump").is_none());
        assert!(component.hooks.get("post:release").is_none());
        assert_eq!(
            component.hooks.get("post:deploy"),
            Some(&vec!["wp cache flush".to_string()])
        );
    }

    #[test]
    fn component_ignores_changelog_targets_alias() {
        let component: Component = serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "changelog_targets": "CHANGELOG.md"
        }))
        .unwrap();

        assert!(component.changelog_target.is_none());
    }

    #[test]
    fn validate_version_target_conflict_same_pattern_ok() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result =
            validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_different_file_ok() {
        let existing = vec![VersionTarget {
            file: "plugin.php".to_string(),
            pattern: Some("Version: (.*)".to_string()),
        }];

        let result = validate_version_target_conflict(
            &existing,
            "package.json",
            "\"version\": \"(.*)\"",
            "test-comp",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_target_conflict_empty_existing_ok() {
        let existing: Vec<VersionTarget> = vec![];

        let result =
            validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_version_pattern_rejects_template_syntax() {
        let result = validate_version_pattern("Version: {version}");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.details.to_string().contains("template syntax"));
    }

    #[test]
    fn validate_version_pattern_rejects_no_capture_group() {
        let result = validate_version_pattern(r"Version: \d+\.\d+\.\d+");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.details.to_string().contains("no capture group"));
    }

    #[test]
    fn validate_version_pattern_rejects_invalid_regex() {
        let result = validate_version_pattern(r"Version: (\d+\.\d+");
        assert!(result.is_err());
    }

    #[test]
    fn validate_supported_build_config_rejects_legacy_build_command() {
        let component = Component {
            id: "sample-codebox".to_string(),
            build_command: Some("npm run package:browser-extension".to_string()),
            ..Default::default()
        };

        let err = component
            .validate_supported_build_config()
            .expect_err("legacy build_command should be unsupported");

        assert!(err.message.contains("unsupported legacy build_command"));
        assert!(err.message.contains("Use scripts.build instead"));
        assert_eq!(err.details["field"].as_str(), Some("build_command"));
        assert!(err.details["tried"].to_string().contains("scripts"));
    }

    #[test]
    fn validate_version_pattern_accepts_valid_pattern() {
        assert!(validate_version_pattern(r"Version:\s*(\d+\.\d+\.\d+)").is_ok());
    }

    #[test]
    fn parse_version_targets_rejects_template_syntax() {
        let targets = vec!["style.css::Version: {version}".to_string()];
        let result = parse_version_targets(&targets);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_version_pattern_converts_double_escaped() {
        // Pattern with double-escaped backslashes (as stored in config)
        let double_escaped = r"Version:\\s*(\\d+\\.\\d+\\.\\d+)";
        let normalized = normalize_version_pattern(double_escaped);
        assert_eq!(normalized, r"Version:\s*(\d+\.\d+\.\d+)");

        // Pattern already correct should stay the same
        let correct = r"Version:\s*(\d+\.\d+\.\d+)";
        let normalized2 = normalize_version_pattern(correct);
        assert_eq!(normalized2, r"Version:\s*(\d+\.\d+\.\d+)");
    }

    #[test]
    fn parse_version_targets_normalizes_double_escaped_patterns() {
        // Simulate pattern stored with double-escaped backslashes
        let targets = vec!["plugin.php::Version:\\s*(\\d+\\.\\d+\\.\\d+)".to_string()];
        let result = parse_version_targets(&targets).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file, "plugin.php");
        assert_eq!(
            result[0].pattern.as_ref().unwrap(),
            r"Version:\s*(\d+\.\d+\.\d+)"
        );
    }

    // ========================================================================
    // Auto-resolve remote_path tests
    // ========================================================================

    #[test]
    fn auto_resolve_remote_path_uses_extension_rule() {
        with_isolated_home(|home| {
            write_extension_fixture(
                home,
                "example",
                r#"{
    "remote_path_inference": [
      {
        "when_file_contains": { "file": "{{dir_name}}.txt", "text": "Deployable" },
        "remote_path": "remote/{{dir_name}}"
      }
    ]
  }"#,
            );

            let dir = home.join("my-component");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("my-component.txt"), "Deployable component").unwrap();

            let component = Component {
                id: "my-component".to_string(),
                local_path: dir.to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "example".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Component::default()
            };

            assert_eq!(
                component.auto_resolve_remote_path(),
                Some("remote/my-component".to_string()),
            );
        });
    }

    #[test]
    fn auto_resolve_remote_path_uses_dirname_not_component_id() {
        with_isolated_home(|home| {
            write_extension_fixture(
                home,
                "example",
                r#"{
    "remote_path_inference": [
      {
        "when_file_contains": { "file": "marker.txt", "text": "Deployable" },
        "remote_path": "remote/{{dir_name}}"
      }
    ]
  }"#,
            );

            let dir = home.join("source-dir");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("marker.txt"), "Deployable component").unwrap();

            let component = Component {
                id: "component-id".to_string(),
                local_path: dir.to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "example".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Component::default()
            };

            assert_eq!(
                component.auto_resolve_remote_path(),
                Some("remote/source-dir".to_string()),
            );
        });
    }

    #[test]
    fn auto_resolve_remote_path_returns_none_without_matching_extension_rule() {
        let component = Component {
            id: "my-crate".to_string(),
            local_path: "/tmp".to_string(),
            extensions: Some(HashMap::from([(
                "rust".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        assert_eq!(component.auto_resolve_remote_path(), None);
    }

    #[test]
    fn auto_resolve_remote_path_returns_none_on_conflicting_extension_rules() {
        with_isolated_home(|home| {
            let rule = |path: &str| {
                format!(
                    r#"{{
    "remote_path_inference": [
      {{
        "when_file_contains": {{ "file": "marker.txt", "text": "Deployable" }},
        "remote_path": "{}"
      }}
    ]
  }}"#,
                    path
                )
            };
            write_extension_fixture(home, "alpha", &rule("remote/alpha/{{dir_name}}"));
            write_extension_fixture(home, "beta", &rule("remote/beta/{{dir_name}}"));

            let dir = home.join("my-component");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("marker.txt"), "Deployable component").unwrap();

            let component = Component {
                id: "my-component".to_string(),
                local_path: dir.to_string_lossy().to_string(),
                extensions: Some(HashMap::from([
                    ("alpha".to_string(), ScopedExtensionConfig::default()),
                    ("beta".to_string(), ScopedExtensionConfig::default()),
                ])),
                ..Component::default()
            };

            assert_eq!(component.auto_resolve_remote_path(), None);
        });
    }

    #[test]
    fn resolve_remote_path_fills_empty() {
        with_isolated_home(|home| {
            write_extension_fixture(
                home,
                "example",
                r#"{
    "remote_path_inference": [
      {
        "when_file_contains": { "file": "marker.txt", "text": "Deployable" },
        "remote_path": "remote/{{dir_name}}"
      }
    ]
  }"#,
            );

            let dir = home.join("my-component");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("marker.txt"), "Deployable component").unwrap();

            let mut component = Component {
                id: "my-component".to_string(),
                local_path: dir.to_string_lossy().to_string(),
                remote_path: String::new(),
                extensions: Some(HashMap::from([(
                    "example".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Component::default()
            };

            component.resolve_remote_path();
            assert_eq!(component.remote_path, "remote/my-component");
        });
    }

    #[test]
    fn resolve_remote_path_preserves_explicit_value() {
        let mut component = Component {
            id: "my-plugin".to_string(),
            local_path: "/tmp".to_string(),
            remote_path: "custom/deploy/path".to_string(),
            extensions: Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        component.resolve_remote_path();
        assert_eq!(component.remote_path, "custom/deploy/path");
    }

    // ========================================================================
    // Portable config discovery tests
    // ========================================================================

    #[test]
    fn discover_from_portable_creates_component_from_homeboy_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let config = serde_json::json!({
            "id": "test-discover",
            "version_targets": [{"file": "Cargo.toml", "pattern": "(?m)^version\\s*=\\s*\"([0-9.]+)\""}],
            "changelog_target": "docs/CHANGELOG.md",
            "extensions": {"rust": {}}
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let result = discover_from_portable(&dir);
        assert!(
            result.is_some(),
            "Should discover component from homeboy.json"
        );

        let comp = result.unwrap();
        assert_eq!(comp.id, "test-discover");
        assert_eq!(comp.local_path, dir.to_string_lossy());
        assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
        assert!(comp
            .extensions
            .as_ref()
            .is_some_and(|m| m.contains_key("rust")));
        assert!(comp.version_targets.is_some());
        assert!(comp.remote_path.is_empty()); // default
    }

    #[test]
    fn discover_from_portable_returns_none_without_homeboy_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        // No homeboy.json in the temp dir

        let result = discover_from_portable(&dir);
        assert!(result.is_none());
    }

    #[test]
    fn discover_from_portable_ignores_machine_specific_in_portable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let config = serde_json::json!({
            "id": "test-machine-fields",
            "local_path": "/wrong/path",
            "remote_path": "/also/wrong",
            "extract_command": "tar -xf artifact.tar.gz"
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let comp = discover_from_portable(&dir).unwrap();
        // id comes from portable JSON
        assert_eq!(comp.id, "test-machine-fields");
        // local_path is derived from actual dir, overriding the portable value
        assert_eq!(comp.local_path, dir.to_string_lossy());
        // remote_path from portable is preserved
        assert_eq!(comp.remote_path, "/also/wrong");
        assert_eq!(
            comp.extract_command.as_deref(),
            Some("tar -xf artifact.tar.gz")
        );
    }

    #[test]
    fn discover_from_portable_with_baselines_and_extensions() {
        // Mirrors data-machine's real homeboy.json — includes subsystem-owned
        // baselines and component-owned extensions. This must not silently fail.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let config = serde_json::json!({
            "auto_cleanup": false,
            "baselines": {
                "lint": {
                    "context_id": "data-machine",
                    "created_at": "2026-03-06T04:47:29Z",
                    "item_count": 0,
                    "known_fingerprints": [],
                    "metadata": {
                        "findings_count": 0
                    }
                }
            },
            "changelog_target": "docs/CHANGELOG.md",
            "extensions": {
                "wordpress": {}
            },
            "id": "data-machine",
            "version_targets": [
                {"file": "data-machine.php", "pattern": "(?m)^\\s*\\*?\\s*Version:\\s*([0-9.]+)"}
            ]
        });
        std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

        let result = discover_from_portable(&dir);
        assert!(
            result.is_some(),
            "Should discover component even with baselines field in homeboy.json"
        );

        let comp = result.unwrap();
        // id comes from portable JSON
        assert_eq!(comp.id, "data-machine");
        assert_eq!(comp.local_path, dir.to_string_lossy());
        // extensions must be present
        assert!(
            comp.extensions.is_some(),
            "extensions should be set from portable config"
        );
        assert!(
            comp.extensions.as_ref().unwrap().contains_key("wordpress"),
            "wordpress extension should be present"
        );
        assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
        assert!(comp.version_targets.is_some());
    }

    #[test]
    fn scoped_extension_config_captures_flat_settings() {
        // Flat keys (the current convention in homeboy.json) must be captured
        // as settings — not silently dropped.
        let json = serde_json::json!({
            "database_type": "mysql",
            "mysql_host": "localhost",
            "mysql_user": "root"
        });

        let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
        assert_eq!(
            config
                .settings
                .get("database_type")
                .and_then(|v| v.as_str()),
            Some("mysql")
        );
        assert_eq!(
            config.settings.get("mysql_host").and_then(|v| v.as_str()),
            Some("localhost")
        );
        assert_eq!(
            config.settings.get("mysql_user").and_then(|v| v.as_str()),
            Some("root")
        );
        assert!(config.version.is_none());
    }

    #[test]
    fn scoped_extension_config_captures_nested_settings() {
        let json = serde_json::json!({
            "version": ">=2.0.0",
            "settings": {
                "database_type": "mysql",
                "mysql_host": "localhost"
            }
        });

        let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.version.as_deref(), Some(">=2.0.0"));
        assert_eq!(
            config
                .settings
                .get("database_type")
                .and_then(|v| v.as_str()),
            Some("mysql")
        );
        assert_eq!(
            config.settings.get("mysql_host").and_then(|v| v.as_str()),
            Some("localhost")
        );
        assert!(config.settings.get("settings").is_none());
    }

    #[test]
    fn scoped_extension_config_flat_settings_override_nested_settings() {
        let json = serde_json::json!({
            "settings": {
                "database_type": "mysql"
            },
            "mysql_host": "localhost",
            "database_type": "sqlite"
        });

        let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
        assert_eq!(
            config
                .settings
                .get("database_type")
                .and_then(|v| v.as_str()),
            Some("sqlite"),
            "flat keys are canonical and must not be overwritten by nested settings"
        );
        assert_eq!(
            config.settings.get("mysql_host").and_then(|v| v.as_str()),
            Some("localhost")
        );
        assert!(config.settings.get("settings").is_none());
    }

    #[test]
    fn component_homeboy_json_routes_nested_wordpress_test_backend_setting() {
        let component: Component = serde_json::from_value(serde_json::json!({
            "id": "roadie",
            "local_path": "/tmp/roadie",
            "extensions": {
                "wordpress": {
                    "settings": {
                        "test_backend": "host-smoke"
                    }
                }
            }
        }))
        .unwrap();

        let wordpress_settings = component
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("wordpress"))
            .expect("wordpress extension config")
            .settings
            .clone();

        assert_eq!(
            wordpress_settings
                .get("test_backend")
                .and_then(|value| value.as_str()),
            Some("host-smoke")
        );
        assert!(wordpress_settings.get("settings").is_none());
    }

    #[test]
    fn scoped_extension_config_serializes_flat_settings() {
        let config = ScopedExtensionConfig {
            version: Some(">=2.0.0".to_string()),
            settings: HashMap::from([("package_manager".to_string(), serde_json::json!("pnpm"))]),
        };

        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json["version"], serde_json::json!(">=2.0.0"));
        assert_eq!(json["package_manager"], serde_json::json!("pnpm"));
        assert!(json.get("settings").is_none());
    }

    #[test]
    fn scoped_extension_config_empty_object() {
        let json = serde_json::json!({});
        let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
        assert!(config.version.is_none());
        assert!(config.settings.is_empty());
    }

    #[test]
    fn scoped_extension_config_version_only() {
        let json = serde_json::json!({ "version": "^1.0" });
        let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.version.as_deref(), Some("^1.0"));
        assert!(config.settings.is_empty());
    }
}
