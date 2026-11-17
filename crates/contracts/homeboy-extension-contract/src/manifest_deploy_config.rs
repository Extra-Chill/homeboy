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
    /// Best-effort runtime cache refresh, inline immediately after publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_switch_command: Option<String>,
    /// Opt-in versioned release directories with an atomic symlink flip.
    /// Absent means the in-place exchange flow (the default for every
    /// extension that does not declare it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_layout: Option<DeployReleaseLayoutPolicy>,
}

/// Versioned release layout for an archive install (#18365).
///
/// Each deploy extracts into
/// `{{targetParentDir}}/<release_root_dir>/{{targetBasename}}/<release-id>/{{targetBasename}}`
/// and the live target path becomes a relative symlink to it, flipped with a
/// single `rename(2)`. Every release keeps a distinct real path, so a request
/// that resolved the old release keeps reading the old tree. Generic: runtime
/// knowledge (for example whether a runtime cache keys files by real path)
/// belongs to the extension's `precondition_command`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeployReleaseLayoutPolicy {
    /// Directory created under the target's parent that holds every release.
    /// Must be a single dot-prefixed path segment, so it shares the target's
    /// filesystem (renames stay O(1)) and directory scanners skip it.
    #[serde(default = "default_release_root_dir")]
    pub release_root_dir: String,
    /// Previous releases to retain, newest first, beyond the active one.
    #[serde(default = "default_keep_previous")]
    pub keep_previous: u32,
    /// Retain any release deactivated less than this many minutes ago.
    #[serde(default = "default_keep_recent_minutes")]
    pub keep_recent_minutes: u32,
    /// Remote command run before the layout is chosen. Exit 0 selects the
    /// release layout; any other exit falls back to the in-place exchange
    /// flow with a deploy warning carrying the command's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition_command: Option<String>,
}

impl DeployReleaseLayoutPolicy {
    /// Reject release roots that could escape the target's parent directory
    /// or become visible to directory scanners.
    pub fn validate(&self) -> Result<(), String> {
        let root = self.release_root_dir.as_str();
        let safe = root.len() > 1
            && root.starts_with('.')
            && root != ".."
            && root
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if !safe {
            return Err(format!(
                "deploy.archive_install.release_layout.release_root_dir must be one dot-prefixed path segment of [A-Za-z0-9._-], got {root:?}"
            ));
        }
        Ok(())
    }
}

impl Default for DeployReleaseLayoutPolicy {
    fn default() -> Self {
        Self {
            release_root_dir: default_release_root_dir(),
            keep_previous: default_keep_previous(),
            keep_recent_minutes: default_keep_recent_minutes(),
            precondition_command: None,
        }
    }
}

fn default_release_root_dir() -> String {
    ".homeboy-releases".to_string()
}

fn default_keep_previous() -> u32 {
    2
}

fn default_keep_recent_minutes() -> u32 {
    15
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
    fn release_layout_defaults_and_validation() {
        let policy: DeployArchiveInstallPolicy = serde_json::from_value(serde_json::json!({
            "path_pattern": "/plugins/",
            "release_layout": { "precondition_command": "check-runtime" }
        }))
        .expect("policy");
        let layout = policy.release_layout.expect("release layout");
        assert_eq!(layout.release_root_dir, ".homeboy-releases");
        assert_eq!(layout.keep_previous, 2);
        assert_eq!(layout.keep_recent_minutes, 15);
        assert_eq!(
            layout.precondition_command.as_deref(),
            Some("check-runtime")
        );
        assert!(layout.validate().is_ok());

        let absent: DeployArchiveInstallPolicy =
            serde_json::from_value(serde_json::json!({ "path_pattern": "/plugins/" }))
                .expect("policy");
        assert!(absent.release_layout.is_none(), "release layout is opt-in");

        for bad in ["releases", ".", "..", ".a/b", "../.x", ".a b", ""] {
            let layout = DeployReleaseLayoutPolicy {
                release_root_dir: bad.to_string(),
                ..Default::default()
            };
            assert!(layout.validate().is_err(), "{bad:?} must be rejected");
        }

        let unknown: Result<DeployArchiveInstallPolicy, _> =
            serde_json::from_value(serde_json::json!({
                "path_pattern": "/plugins/",
                "release_layout": { "keep": 3 }
            }));
        assert!(
            unknown.is_err(),
            "unknown release layout fields are rejected"
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
