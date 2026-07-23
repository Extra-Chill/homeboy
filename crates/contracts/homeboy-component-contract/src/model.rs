use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::config::{
    is_default_github_config, ArtifactInput, CleanupArtifactDeclaration, ComponentDeployConfig,
    ComponentReleaseConfig, ComponentScriptsConfig, DependencyStackEdge, GitDeployConfig,
    GithubConfig, ScopeConfig, ScopedExtensionConfig, VersionTarget,
};
use homeboy_audit_contract::AuditConfig;
use homeboy_engine_primitives::canonical_json::canonical_json;

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
    /// Explicit extension ownership by capability label.
    ///
    /// When multiple linked extensions advertise the same capability, Homeboy
    /// requires the component to choose the owner here instead of guessing.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub capability_extensions: HashMap<String, String>,
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
    /// Component-scoped environment variables applied to Homeboy-managed
    /// capability runs for this component. Per-run env overrides win on conflict.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
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
    /// Declarative reviewer-body policy for agent-task PR finalization.
    ///
    /// Carried opaquely as JSON so the core component model does not depend on
    /// the agent-task subsystem; the agent-task review-dossier layer
    /// deserializes it into its `AgentTaskReviewProfile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_profile: Option<serde_json::Value>,
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    capability_extensions: HashMap<String, String>,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review_profile: Option<serde_json::Value>,
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
            capability_extensions: raw.capability_extensions,
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
            env: raw.env,
            audit: raw.audit,
            dependency_stack: raw.dependency_stack,
            deploy_together: raw.deploy_together,
            artifact_inputs: raw.artifact_inputs,
            cli_path: raw.cli_path,
            extra_drift_files: raw.extra_drift_files,
            cleanup_artifacts: raw.cleanup_artifacts,
            baselines: raw.baselines,
            audit_rules: raw.audit_rules,
            review_profile: raw.review_profile,
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
            capability_extensions: c.capability_extensions,
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
            env: c.env,
            audit: c.audit,
            dependency_stack: c.dependency_stack,
            deploy_together: c.deploy_together,
            artifact_inputs: c.artifact_inputs,
            cli_path: c.cli_path,
            extra_drift_files: c.extra_drift_files,
            cleanup_artifacts: c.cleanup_artifacts,
            baselines: c.baselines,
            audit_rules: c.audit_rules,
            review_profile: c.review_profile,
        }
    }
}

impl Component {
    /// Return a stable serialization suitable for comparing resolved component
    /// configuration across independently loaded snapshots.
    pub fn canonical_identity(&self) -> serde_json::Result<String> {
        serde_json::to_value(self)
            .and_then(|value| serde_json::to_string(&canonical_json(value)))
    }

    /// Return a stable identity for comparing a component's *configured*
    /// attachment across independently loaded snapshots — release-set preflight
    /// vs. deploy execution (#9205).
    ///
    /// Excludes `build_artifact`, which is a deterministic, execution-only
    /// derivation of `local_path` + the configured artifact (deploy planning
    /// resolves it to an absolute path via `resolve_path_string`, while
    /// preflight captures the pre-resolution component). Comparing the raw
    /// serialization therefore reported a clean attachment as "changed after
    /// release-set preflight" even though no configuration drifted. Real
    /// attachment/config/path drift still changes every remaining field, so the
    /// drift guard keeps its fail-closed intent.
    pub fn canonical_attachment_identity(&self) -> serde_json::Result<String> {
        let mut value = serde_json::to_value(self)?;
        if let Some(object) = value.as_object_mut() {
            object.remove("build_artifact");
        }
        serde_json::to_string(&canonical_json(value))
    }

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
            capability_extensions: HashMap::new(),
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
            env: BTreeMap::new(),
            audit: None,
            dependency_stack: Vec::new(),
            deploy_together: Vec::new(),
            artifact_inputs: Vec::new(),
            cli_path: None,
            extra_drift_files: Vec::new(),
            cleanup_artifacts: Vec::new(),
            baselines: None,
            audit_rules: None,
            review_profile: None,
        }
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
        capability: homeboy_extension_contract::ExtensionCapability,
    ) -> &[String] {
        if let Some(scripts) = self.scripts.as_ref() {
            let commands = match capability {
                homeboy_extension_contract::ExtensionCapability::Lint => &scripts.lint,
                homeboy_extension_contract::ExtensionCapability::Test => &scripts.test,
                homeboy_extension_contract::ExtensionCapability::Build => &scripts.build,
                homeboy_extension_contract::ExtensionCapability::Bench => &scripts.bench,
                homeboy_extension_contract::ExtensionCapability::Fuzz => &scripts.fuzz,
                homeboy_extension_contract::ExtensionCapability::Trace => &scripts.trace,
                homeboy_extension_contract::ExtensionCapability::Deps => &scripts.deps,
            };
            if !commands.is_empty() {
                return commands;
            }
        }

        &[]
    }

    pub fn has_script(&self, capability: homeboy_extension_contract::ExtensionCapability) -> bool {
        !self.script_commands(capability).is_empty()
    }

    pub fn validate_supported_build_config(&self) -> homeboy_error::Result<()> {
        let Some(command) = self
            .build_command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        else {
            return Ok(());
        };

        Err(homeboy_error::Error::validation_invalid_argument(
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
}

/// Render an extension remote-path template against a component id + on-disk
/// directory name. Pure string substitution; the extension-driven auto-resolve
/// behavior that consumes it lives in `homeboy-core`.
pub fn render_remote_path_template(template: &str, component_id: &str, dir_name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_attachment_identity_ignores_build_artifact_but_catches_config_drift() {
        let base = Component::new(
            "agents-api".to_string(),
            "/source/agents-api".to_string(),
            "wp-content/plugins/agents-api".to_string(),
            Some("dist/plugin.zip".to_string()),
        );

        // Same attachment, but build_artifact resolved to an absolute path during
        // execution (the #9205 divergence). Attachment identity must be equal.
        let mut resolved = base.clone();
        resolved.build_artifact = Some("/source/agents-api/dist/plugin.zip".to_string());
        assert_eq!(
            base.canonical_attachment_identity().expect("base identity"),
            resolved
                .canonical_attachment_identity()
                .expect("resolved identity"),
            "build_artifact resolution must not change the attachment identity"
        );

        // The raw canonical_identity still differs (build_artifact is included),
        // proving the attachment variant is what makes them agree.
        assert_ne!(
            base.canonical_identity().expect("base"),
            resolved.canonical_identity().expect("resolved"),
            "raw canonical_identity should still reflect the build_artifact difference"
        );

        // A real config change (different remote_path) still drifts.
        let mut drifted = base.clone();
        drifted.remote_path = "wp-content/plugins/changed".to_string();
        assert_ne!(
            base.canonical_attachment_identity().expect("base"),
            drifted.canonical_attachment_identity().expect("drifted"),
            "real config drift must still change the attachment identity"
        );
    }
}
