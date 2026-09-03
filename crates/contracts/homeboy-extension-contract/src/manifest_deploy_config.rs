use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployArchiveInstallPolicy {
    pub path_pattern: String,
    #[serde(default = "default_staging_path")]
    pub staging_path: String,
    #[serde(default)]
    pub root_must_match_target_basename: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_header: Option<DeployRequiredHeader>,
    #[serde(default)]
    pub skip_permissions_fix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "DeployRequiredHeaderConfig")]
pub struct DeployRequiredHeader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_glob: Option<String>,
    pub contains: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeployRequiredHeaderConfig {
    pub file: Option<String>,
    pub file_glob: Option<String>,
    pub contains: String,
}

impl TryFrom<DeployRequiredHeaderConfig> for DeployRequiredHeader {
    type Error = String;

    fn try_from(config: DeployRequiredHeaderConfig) -> Result<Self, Self::Error> {
        if config.file.is_some() == config.file_glob.is_some() {
            return Err(
                "deploy.archive_install.required_header must declare exactly one of file or file_glob"
                    .to_string(),
            );
        }

        Ok(Self {
            file: config.file,
            file_glob: config.file_glob,
            contains: config.contains,
        })
    }
}

fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}

/// Generic deployment provider descriptor owned by an extension manifest.
///
/// This was carried in `ExtensionManifest::extra` until #13723. A malformed
/// descriptor there deserialized to `None`, was discarded, and presented as
/// zero declared providers with no diagnostic; as a typed field it is a named
/// deserialization error instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentProviderManifest {
    pub id: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layered_input: Option<DeploymentProviderLayeredInputManifest>,
}

/// Opt-in capability for providers that receive a versioned layered payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentProviderLayeredInputManifest {
    pub schema: String,
    #[serde(default)]
    pub target_required: bool,
    /// Schema for provider-produced structured evidence. Providers declaring this
    /// contract are responsible for emitting already-redacted JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<String>,
}

pub const DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA: &str = "homeboy/deployment-provider-payload/v1";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtensionManifest;

    #[test]
    fn reads_multiple_generic_provider_descriptors() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "fixture", "version": "1.0.0",
            "deployment_providers": [
                { "id": "fixture.alpha", "command": "fixture-alpha --contract {{payload.contract}}" },
                { "id": "fixture.beta", "command": "fixture-beta --contract {{payload.contract}}", "dry_run_command": "fixture-beta --dry-run --contract {{payload.contract}}", "layered_input": { "schema": "homeboy/deployment-provider-payload/v1", "target_required": true, "result_schema": "fixture/deployment-result/v1" } }
            ]
        }))
        .expect("fixture manifest");

        assert_eq!(manifest.deployment_providers.len(), 2);
        assert_eq!(manifest.deployment_providers[0].id, "fixture.alpha");
        assert_eq!(manifest.deployment_providers[1].id, "fixture.beta");
        assert_eq!(
            manifest.deployment_providers[1]
                .layered_input
                .as_ref()
                .expect("layered input")
                .schema,
            DEPLOYMENT_PROVIDER_PAYLOAD_SCHEMA
        );
    }

    #[test]
    fn malformed_provider_descriptor_is_a_named_manifest_error() {
        let result: Result<ExtensionManifest, _> = serde_json::from_value(serde_json::json!({
            "name": "fixture", "version": "1.0.0",
            "deployment_providers": [{ "id": "fixture.alpha" }]
        }));

        let error = result.expect_err("missing command must not deserialize");
        assert!(error.to_string().contains("command"));
    }
}
