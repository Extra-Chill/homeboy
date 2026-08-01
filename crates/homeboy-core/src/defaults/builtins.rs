use super::{
    DeployConfig, InstallMethodConfig, InstallMethodsConfig, PermissionModes, PermissionsConfig,
    VersionCandidateConfig,
};
use homeboy_extension_contract::TestDriftConfig;
use serde::Deserialize;
use std::fs;
use std::sync::OnceLock;

/// Defaults that an ecosystem extension may supply, with a generic fallback
/// compiled into core.
///
/// Two documents implement this same schema, and they are deliberately NOT
/// copies of each other (#11099):
///
/// * The bundled asset under this crate's `assets/defaults/` is core's
///   fallback. It is framework-agnostic on purpose (#2240): only generic
///   dev-tool manifests, a generic source layout, and no presupposed file
///   extension. Four tests at the bottom of this file pin that agnosticism.
/// * The document owned by the extensions repository is an ecosystem
///   *override*, loaded at runtime via the `EXTENSION_DEFAULTS_PATH`
///   environment variable. It intentionally carries the extra
///   framework-specific candidates, source directories, and file extensions
///   that core must not bake in.
///
/// So a field-by-field difference between the two is the design working, not
/// drift. The one field where they must agree is
/// `install_methods.source.upgrade_command`, which describes how *Homeboy
/// itself* is rebuilt and has nothing to do with any ecosystem; the override
/// copy is currently stale there and is tracked for a follow-up in the
/// extensions repository.
#[derive(Debug, Clone, Deserialize)]
struct ExtensionProvidedDefaults {
    install_methods: InstallMethodsConfig,
    version_candidates: Vec<VersionCandidateConfig>,
    test_drift: TestDriftConfig,
    direct_test_file_suffixes: Vec<String>,
}

fn extension_provided_defaults() -> &'static ExtensionProvidedDefaults {
    static DEFAULTS: OnceLock<ExtensionProvidedDefaults> = OnceLock::new();

    DEFAULTS.get_or_init(load_extension_provided_defaults)
}

/// Resolve the active defaults: an operator-supplied override when one is
/// configured, otherwise the compiled-in bundled asset.
///
/// The bundled asset is load-bearing and cannot be reduced to an empty
/// `Default` the way the detector profile was. `install_methods` drives
/// `detect_install_method_from_exe_path`, which matches the running
/// executable's path against `path_patterns`. With empty defaults every
/// pattern list is empty, detection returns `InstallMethod::Unknown`, and
/// `homeboy upgrade` fails outright with "Cannot upgrade: unknown
/// installation method". Nothing sets `EXTENSION_DEFAULTS_PATH`
/// automatically, so an install with no override configured would have no
/// other source for these values — self-upgrade is a bootstrap path and must
/// keep working with zero extensions installed.
fn load_extension_provided_defaults() -> ExtensionProvidedDefaults {
    if let Some(defaults) = load_external_extension_provided_defaults() {
        return defaults;
    }

    parse_extension_provided_defaults(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/defaults/extension-provided-defaults.json"
    )))
}

/// Load an operator-supplied defaults override, or `None` to fall back to the
/// bundled asset.
///
/// Every failure mode here is operator input, so all of them fall back rather
/// than abort: an unset variable, an unreadable file, and a file that does not
/// parse as a defaults document. The parse case previously panicked through
/// `expect`, which made a single malformed override file take down every
/// command that touches defaults — including the `upgrade` path that would
/// otherwise repair the install.
fn load_external_extension_provided_defaults() -> Option<ExtensionProvidedDefaults> {
    let path =
        std::env::var(crate::product_identity::PRODUCT_IDENTITY.env_var("EXTENSION_DEFAULTS_PATH"))
            .ok()?;
    let content = fs::read_to_string(path).ok()?;

    try_parse_extension_provided_defaults(&content)
}

/// Parse a defaults document supplied at runtime, returning `None` when it is
/// not a valid defaults contract.
fn try_parse_extension_provided_defaults(content: &str) -> Option<ExtensionProvidedDefaults> {
    serde_json::from_str(content).ok()
}

/// Parse a defaults document that is expected to be well-formed, panicking if
/// it is not. Reserved for the compiled-in bundled asset, where a parse failure
/// is a build defect rather than bad operator input and should stay loud.
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

pub fn extension_provided_test_drift_config() -> TestDriftConfig {
    extension_provided_defaults().test_drift.clone()
}

pub fn extension_provided_direct_test_file_suffixes() -> Vec<String> {
    extension_provided_defaults()
        .direct_test_file_suffixes
        .clone()
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
    crate::product_identity::PRODUCT_IDENTITY
        .artifact_prefix
        .to_string()
}

pub fn deploy_generated_build_dir() -> String {
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
        dir_mode: "g+ws".to_string(),
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
        assert!(!suffixes.contains(&"Test.php".to_string()));
        assert!(suffixes.contains(&".test.js".to_string()));
        assert!(suffixes.contains(&".spec.tsx".to_string()));
        assert!(suffixes.contains(&"_test.rs".to_string()));
    }

    #[test]
    fn malformed_runtime_defaults_document_falls_back_to_bundled_asset() {
        // An override file is untrusted operator input. A document that does
        // not parse must yield None so the bundled asset stays in effect,
        // rather than aborting every command that reads defaults — including
        // the upgrade path that would otherwise repair the install.
        assert!(try_parse_extension_provided_defaults("not a defaults document").is_none());
        assert!(try_parse_extension_provided_defaults("{}").is_none());
    }

    #[test]
    fn well_formed_runtime_defaults_document_overrides_bundled_asset() {
        let defaults = try_parse_extension_provided_defaults(
            r#"{
                "install_methods": {},
                "version_candidates": [],
                "test_drift": {
                    "source_dirs": ["lib"],
                    "test_dirs": ["spec"],
                    "file_extensions": ["rb"],
                    "inline_tests": true
                },
                "direct_test_file_suffixes": ["_spec.rb"]
            }"#,
        )
        .expect("well-formed defaults document parses");

        assert_eq!(defaults.test_drift.source_dirs, ["lib"]);
        assert_eq!(defaults.test_drift.test_dirs, ["spec"]);
        assert_eq!(defaults.direct_test_file_suffixes, ["_spec.rb"]);
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
