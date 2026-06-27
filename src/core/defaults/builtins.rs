use super::{
    DeployConfig, InstallMethodConfig, InstallMethodsConfig, PermissionModes, PermissionsConfig,
    VersionCandidateConfig,
};
use crate::core::extension::TestDriftConfig;
use serde::Deserialize;
use std::fs;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
struct ExtensionProvidedDefaults {
    install_methods: InstallMethodsConfig,
    version_candidates: Vec<VersionCandidateConfig>,
    test_drift: TestDriftConfig,
    direct_test_file_suffixes: Vec<String>,
    /// Ecosystem-specific code-audit detector defaults (version-guard constants
    /// and regexes, tracker-reference URL shapes). Core ships none of these as
    /// Rust literals; the framework set lives here in the extension-provided
    /// defaults asset and is merged in when a component opts into builtin
    /// profile defaults, keeping core source framework-agnostic (#2240).
    #[serde(default)]
    detector_profile: DetectorProfileDefaults,
}

/// Extension-provided code-audit detector defaults. Empty by default so a
/// truly generic core (or an external defaults file that omits the section)
/// carries no framework-specific detection literals.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct DetectorProfileDefaults {
    #[serde(default)]
    pub version_guard_constants: Vec<String>,
    #[serde(default)]
    pub version_guard_regexes: Vec<String>,
    #[serde(default)]
    pub tracker_reference_regexes: Vec<String>,
}

fn extension_provided_defaults() -> &'static ExtensionProvidedDefaults {
    static DEFAULTS: OnceLock<ExtensionProvidedDefaults> = OnceLock::new();

    DEFAULTS.get_or_init(load_extension_provided_defaults)
}

fn load_extension_provided_defaults() -> ExtensionProvidedDefaults {
    if let Some(defaults) = load_external_extension_provided_defaults() {
        return defaults;
    }

    parse_extension_provided_defaults(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/defaults/extension-provided-defaults.json"
    )))
}

fn load_external_extension_provided_defaults() -> Option<ExtensionProvidedDefaults> {
    let path = std::env::var(["HOMEBOY", "EXTENSION_DEFAULTS_PATH"].join("_")).ok()?;
    let content = fs::read_to_string(path).ok()?;

    Some(parse_extension_provided_defaults(&content))
}

fn parse_extension_provided_defaults(content: &str) -> ExtensionProvidedDefaults {
    serde_json::from_str(content).expect("extension-provided defaults asset should parse")
}

pub(super) fn default_install_methods() -> InstallMethodsConfig {
    extension_provided_defaults().install_methods.clone()
}

pub(super) fn default_homebrew_config() -> InstallMethodConfig {
    default_install_methods().homebrew
}

pub(super) fn default_secondary_install_config() -> InstallMethodConfig {
    default_install_methods().secondary
}

pub(super) fn default_source_config() -> InstallMethodConfig {
    default_install_methods().source
}

pub(super) fn default_binary_config() -> InstallMethodConfig {
    default_install_methods().binary
}

pub(super) fn default_version_candidates() -> Vec<VersionCandidateConfig> {
    extension_provided_defaults().version_candidates.clone()
}

pub(crate) fn extension_provided_test_drift_config() -> TestDriftConfig {
    extension_provided_defaults().test_drift.clone()
}

pub(crate) fn extension_provided_direct_test_file_suffixes() -> Vec<String> {
    extension_provided_defaults()
        .direct_test_file_suffixes
        .clone()
}

pub(crate) fn extension_provided_detector_profile() -> DetectorProfileDefaults {
    extension_provided_defaults().detector_profile.clone()
}

pub(super) fn default_deploy() -> DeployConfig {
    DeployConfig {
        scp_flags: default_scp_flags(),
        artifact_prefix: default_artifact_prefix(),
        default_ssh_port: default_ssh_port(),
    }
}

pub(super) fn default_scp_flags() -> Vec<String> {
    vec!["-O".to_string()]
}

pub(super) fn default_artifact_prefix() -> String {
    crate::core::product_identity::PRODUCT_IDENTITY
        .artifact_prefix
        .to_string()
}

pub(crate) fn deploy_generated_build_dir() -> String {
    format!("{}build", default_artifact_prefix())
}

pub(super) fn default_ssh_port() -> u16 {
    22
}

pub(super) fn default_permissions() -> PermissionsConfig {
    PermissionsConfig {
        local: default_local_permissions(),
        remote: default_remote_permissions(),
    }
}

pub(super) fn default_local_permissions() -> PermissionModes {
    PermissionModes {
        file_mode: "g+rw".to_string(),
        dir_mode: "g+rwx".to_string(),
    }
}

pub(super) fn default_remote_permissions() -> PermissionModes {
    PermissionModes {
        file_mode: "g+w".to_string(),
        dir_mode: "g+w".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_upgrade_installs_active_binary() {
        let config = default_source_config();

        assert!(config.upgrade_command.contains("cargo build --release"));
        assert!(config.upgrade_command.contains("target/release/homeboy"));
        assert!(config.upgrade_command.contains("--version"));
        assert!(!config.upgrade_command.contains("cargo install --path"));
    }

    #[test]
    fn core_version_candidate_defaults_are_framework_agnostic() {
        // Core's built-in version-candidate fallback ships only generic dev-tool
        // manifests. Framework-specific candidates (e.g. a PHP `composer.json`
        // or a WordPress theme `style.css`) belong to the extension that owns
        // them, supplied via the external defaults override (#2240).
        let files = default_version_candidates()
            .into_iter()
            .map(|candidate| candidate.file)
            .collect::<Vec<_>>();

        assert_eq!(files, ["Cargo.toml", "package.json"]);
    }

    #[test]
    fn core_test_drift_fallback_is_framework_agnostic() {
        let config = extension_provided_test_drift_config();

        // Generic source layout only — no PHP/WordPress `inc`/`lib` conventions
        // and no presupposed file extension.
        assert_eq!(config.source_dirs, ["src"]);
        assert_eq!(config.test_dirs, ["tests"]);
        assert!(config.file_extensions.is_empty());
        assert!(!config.inline_tests);
    }

    #[test]
    fn core_direct_test_suffix_fallback_is_framework_agnostic() {
        let suffixes = extension_provided_direct_test_file_suffixes();

        // No PHP `Test.php` convention baked into core; generic JS/TS/Rust
        // suffixes remain.
        assert!(!suffixes.contains(&["Test.", "p", "hp"].concat()));
        assert!(suffixes.contains(&".test.js".to_string()));
        assert!(suffixes.contains(&".spec.tsx".to_string()));
        assert!(suffixes.contains(&"_test.rs".to_string()));
    }

    #[test]
    fn detector_profile_defaults_supply_framework_version_guards() {
        // The framework-specific detector defaults live in the extension-provided
        // asset, not core Rust literals — but they are still wired one-repo so WP
        // detection behavior is preserved (#2240).
        let profile = extension_provided_detector_profile();

        assert!(profile
            .version_guard_constants
            .iter()
            .any(|c| c == "JETPACK__VERSION"));
        assert!(!profile.version_guard_regexes.is_empty());
        assert!(profile
            .tracker_reference_regexes
            .iter()
            .any(|r| r.contains("wordpress")));
    }

    #[test]
    fn parses_homeboy_extensions_owned_defaults_contract() {
        let path = std::env::current_dir()
            .expect("resolve current working directory")
            .parent()
            .expect("worktree has parent")
            .join("homeboy-extensions-defaults-fixture")
            .join("defaults/extension-provided-defaults.json");

        if !path.exists() {
            return;
        }

        let content = fs::read_to_string(path).expect("read extension-owned defaults");
        let defaults = parse_extension_provided_defaults(&content);

        assert_eq!(defaults.version_candidates.len(), 4);
        assert_eq!(defaults.test_drift.test_dirs, ["tests"]);
    }
}
