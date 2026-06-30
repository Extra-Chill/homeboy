use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};

use super::audit::AuditConfig;
use super::config::{
    is_default_github_config, ArtifactInput, CleanupArtifactDeclaration, ComponentDeployConfig,
    ComponentReleaseConfig, ComponentScriptsConfig, DependencyStackEdge, GitDeployConfig,
    GithubConfig, ScopeConfig, ScopedExtensionConfig, VersionTarget,
};

/// Lifecycle state of a component.
///
/// Components are normally `Active` — independently versioned and deployable.
/// When a formerly-standalone component is absorbed into another component
/// (`Bundled`) or sunset entirely (`Retired`), Homeboy must stop treating it
/// as an independent deploy obligation: it is suppressed from `--outdated`
/// reports, deploy selection, and standalone version checks. This avoids
/// chasing false deploy drift and accidentally re-shipping a stale standalone
/// plugin that no longer owns the runtime contract.
///
/// This is config-driven and generic — no specific component names are
/// hardcoded in core. A bundled component declares its host via
/// [`Component::bundled_into`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ComponentLifecycle {
    /// Independently versioned and deployable (the default).
    #[default]
    Active,
    /// Absorbed into another component; its version tracks the host.
    /// Not independently deployable. See [`Component::bundled_into`].
    Bundled,
    /// Sunset entirely; no longer deployable and excluded from all
    /// standalone version/deploy/outdated surfaces.
    Retired,
}

impl ComponentLifecycle {
    /// True only for `Active`. `Bundled` and `Retired` components are not
    /// independently deployable.
    pub fn is_active(self) -> bool {
        matches!(self, ComponentLifecycle::Active)
    }

    /// Human-readable reason for why a non-active component is suppressed.
    pub fn suppression_reason(self) -> Option<&'static str> {
        match self {
            ComponentLifecycle::Active => None,
            ComponentLifecycle::Bundled => Some("Component is bundled into another component"),
            ComponentLifecycle::Retired => Some("Component is retired"),
        }
    }
}

fn is_active_lifecycle(lifecycle: &ComponentLifecycle) -> bool {
    lifecycle.is_active()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(from = "RawComponent", into = "RawComponent")]
pub struct Component {
    pub id: String,
    pub aliases: Vec<String>,
    pub local_path: String,
    pub remote_path: String,
    /// Lifecycle state. `Active` (default) means independently deployable;
    /// `Bundled`/`Retired` suppress the component from deploy/outdated/version
    /// surfaces. See [`ComponentLifecycle`].
    pub lifecycle: ComponentLifecycle,
    /// When `lifecycle` is `Bundled`, the component this one was absorbed into.
    /// Informational/reporting only — its version tracks the host component.
    pub bundled_into: Option<String>,
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
    /// Host-scoped GitHub CLI/API environment used by release automation.
    #[serde(default, skip_serializing_if = "is_default_github_config")]
    pub github: GithubConfig,
    /// Release ownership metadata used to guard sharp release execution modes.
    #[serde(default, skip_serializing_if = "ComponentReleaseConfig::is_default")]
    pub release: ComponentReleaseConfig,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_stack: Vec<DependencyStackEdge>,
    /// Component IDs that must be deployed in the same operation as this component.
    ///
    /// Use this for separately tracked components that form one runtime contract,
    /// such as a plugin and theme that must stay in sync on the target site.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploy_together: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_inputs: Vec<ArtifactInput>,
    /// Override the CLI path used by extension deploy install steps.
    /// For example, local wrappers may need "lando wp" instead of the default "wp".
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
    #[serde(default, skip_serializing_if = "is_active_lifecycle")]
    lifecycle: ComponentLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bundled_into: Option<String>,
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
    #[serde(default, skip_serializing_if = "is_default_github_config")]
    github: GithubConfig,
    #[serde(default, skip_serializing_if = "ComponentReleaseConfig::is_default")]
    release: ComponentReleaseConfig,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dependency_stack: Vec<DependencyStackEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deploy_together: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifact_inputs: Vec<ArtifactInput>,
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
            lifecycle: raw.lifecycle,
            bundled_into: raw.bundled_into,
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
            github: raw.github,
            release: raw.release,
            triage_remote_url: raw.triage_remote_url,
            priority_labels: raw.priority_labels,
            auto_cleanup: raw.auto_cleanup,
            docs_dir: raw.docs_dir,
            docs_dirs: raw.docs_dirs,
            scopes: raw.scopes,
            scripts: raw.scripts,
            audit: raw.audit,
            dependency_stack: raw.dependency_stack,
            deploy_together: raw.deploy_together,
            artifact_inputs: raw.artifact_inputs,
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
            lifecycle: c.lifecycle,
            bundled_into: c.bundled_into,
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
            github: c.github,
            release: c.release,
            triage_remote_url: c.triage_remote_url,
            priority_labels: c.priority_labels,
            auto_cleanup: c.auto_cleanup,
            docs_dir: c.docs_dir,
            docs_dirs: c.docs_dirs,
            scopes: c.scopes,
            scripts: c.scripts,
            audit: c.audit,
            dependency_stack: c.dependency_stack,
            deploy_together: c.deploy_together,
            artifact_inputs: c.artifact_inputs,
            cli_path: c.cli_path,
            extra_drift_files: c.extra_drift_files,
            cleanup_artifacts: c.cleanup_artifacts,
            baselines: c.baselines,
            audit_rules: c.audit_rules,
        }
    }
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
            lifecycle: ComponentLifecycle::default(),
            bundled_into: None,
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
            github: GithubConfig::default(),
            release: ComponentReleaseConfig::default(),
            triage_remote_url: None,
            priority_labels: None,
            auto_cleanup: false,
            docs_dir: None,
            docs_dirs: Vec::new(),
            scopes: None,
            scripts: None,
            audit: None,
            dependency_stack: Vec::new(),
            deploy_together: Vec::new(),
            artifact_inputs: Vec::new(),
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

    /// Whether this component is independently deployable per its lifecycle.
    ///
    /// `Bundled` and `Retired` components are not independently deployable —
    /// they are suppressed from deploy selection, `--outdated` reports, and
    /// standalone version checks. Callers should skip them rather than treat
    /// them as deploy obligations.
    pub fn is_active_lifecycle(&self) -> bool {
        self.lifecycle.is_active()
    }

    /// Reason a non-active component is suppressed from deploy/outdated/version
    /// surfaces, or `None` when the component is active. For `Bundled`
    /// components the host (from `bundled_into`) is included when known.
    pub fn lifecycle_suppression_reason(&self) -> Option<String> {
        let base = self.lifecycle.suppression_reason()?;
        match (self.lifecycle, self.bundled_into.as_deref()) {
            (ComponentLifecycle::Bundled, Some(host)) => {
                Some(format!("Component is bundled into '{host}'"))
            }
            _ => Some(base.to_string()),
        }
    }

    /// Check if this component's local_path points to a file (not a directory).
    ///
    /// File components use `deploy_strategy: "file"` and are deployed via
    /// atomic SCP instead of rsync. They skip build, git sync, and tag checkout.
    pub fn is_file_component(&self) -> bool {
        self.deploy_config().is_file_deploy()
            || (std::path::Path::new(&self.local_path).is_file()
                && self.deploy_strategy().is_none())
    }

    pub fn deploy_config(&self) -> ComponentDeployConfig<'_> {
        ComponentDeployConfig {
            local_path: &self.local_path,
            remote_path: &self.remote_path,
            build_artifact: self.build_artifact.as_deref(),
            extract_command: self.extract_command.as_deref(),
            remote_owner: self.remote_owner.as_deref(),
            deploy_strategy: self.deploy_strategy.as_deref(),
            git_deploy: self.git_deploy.as_ref(),
            remote_url: self.remote_url.as_deref(),
            cli_path: self.cli_path.as_deref(),
            artifact_inputs: &self.artifact_inputs,
            cleanup_artifacts: &self.cleanup_artifacts,
        }
    }

    pub fn deploy_strategy(&self) -> Option<&str> {
        self.deploy_config().deploy_strategy
    }

    pub fn git_deploy_config(&self) -> Option<&GitDeployConfig> {
        self.deploy_config().git_deploy
    }

    pub fn remote_url(&self) -> Option<&str> {
        self.deploy_config().remote_url
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
                crate::core::extension::ExtensionCapability::Fuzz => &scripts.fuzz,
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
