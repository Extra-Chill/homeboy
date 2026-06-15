use super::{
    DeployConfig, InstallMethodConfig, InstallMethodsConfig, PermissionModes, PermissionsConfig,
    VersionCandidateConfig,
};
use crate::core::extension::TestDriftConfig;
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
struct ExtensionProvidedDefaults {
    install_methods: InstallMethodsConfig,
    version_candidates: Vec<VersionCandidateConfig>,
    test_drift: TestDriftConfig,
    direct_test_file_suffixes: Vec<String>,
}

fn extension_provided_defaults() -> &'static ExtensionProvidedDefaults {
    static DEFAULTS: OnceLock<ExtensionProvidedDefaults> = OnceLock::new();

    DEFAULTS.get_or_init(|| {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/defaults/extension-provided-defaults.json"
        )))
        .expect("extension-provided defaults asset should parse")
    })
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

        assert!(config
            .upgrade_command
            .contains("cargo install --path . --force"));
        assert!(!config.upgrade_command.contains("cargo build --release"));
    }

    #[test]
    fn extension_provided_defaults_preserve_existing_version_candidates() {
        let files = default_version_candidates()
            .into_iter()
            .map(|candidate| candidate.file)
            .collect::<Vec<_>>();

        assert_eq!(
            files,
            ["Cargo.toml", "package.json", "composer.json", "style.css"]
        );
    }

    #[test]
    fn extension_provided_defaults_preserve_existing_test_drift_fallback() {
        let config = extension_provided_test_drift_config();

        assert_eq!(config.source_dirs, ["src", "inc", "lib"]);
        assert_eq!(config.test_dirs, ["tests"]);
        assert_eq!(config.file_extensions, [["p", "hp"].concat()]);
        assert!(!config.inline_tests);
    }

    #[test]
    fn extension_provided_defaults_preserve_existing_direct_test_suffixes() {
        let suffixes = extension_provided_direct_test_file_suffixes();

        assert!(suffixes.contains(&["Test.", "p", "hp"].concat()));
        assert!(suffixes.contains(&".test.js".to_string()));
        assert!(suffixes.contains(&".spec.tsx".to_string()));
    }
}
