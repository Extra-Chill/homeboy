use std::collections::HashMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::core::component::Component;
use crate::core::error::{Error, Result};

use super::release_download::ReleaseArtifactLease;
use super::{DeployConfig, PreparedDeployArtifact};

/// Immutable, target-independent inputs used to prepare a component payload.
///
/// Remote paths, install strategies, hooks, SSH, lifecycle, and target policy
/// deliberately have no representation here.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ComponentPayloadPreparationRequest {
    pub component: ComponentPayloadPreparationSource,
    pub config: PreparationConfig,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ComponentPayloadPreparationSource {
    pub id: String,
    pub local_path: String,
    pub build_artifact: Option<String>,
    pub build_command: Option<String>,
    pub extensions:
        Option<std::collections::HashMap<String, crate::core::component::ScopedExtensionConfig>>,
    pub capability_extensions: std::collections::HashMap<String, String>,
    pub scopes: Option<crate::core::component::ScopeConfig>,
    pub artifact_inputs: Vec<crate::core::component::ArtifactInput>,
    pub scripts: Option<crate::core::component::ComponentScriptsConfig>,
    pub env: std::collections::BTreeMap<String, String>,
    pub remote_url: Option<String>,
    pub github: crate::core::component::GithubConfig,
    pub release: crate::core::component::ComponentReleaseConfig,
}

impl ComponentPayloadPreparationRequest {
    pub fn new(component: &Component, config: &DeployConfig) -> Self {
        Self {
            component: ComponentPayloadPreparationSource {
                id: component.id.clone(),
                local_path: component.local_path.clone(),
                build_artifact: component.build_artifact.clone(),
                build_command: component.build_command.clone(),
                extensions: component.extensions.clone(),
                capability_extensions: component.capability_extensions.clone(),
                scopes: component.scopes.clone(),
                artifact_inputs: component.artifact_inputs.clone(),
                scripts: component.scripts.clone(),
                env: component.env.clone(),
                remote_url: component.remote_url.clone(),
                github: component.github.clone(),
                release: component.release.clone(),
            },
            config: PreparationConfig::from(config),
        }
    }

    /// Stable evidence and deduplication key for every source/build input.
    pub fn identity(&self) -> String {
        digest_json(&canonical_json(
            serde_json::to_value(self).expect("preparation request serializes"),
        ))
    }

    pub fn evidence(&self) -> serde_json::Value {
        let mut evidence = serde_json::to_value(self).expect("preparation request serializes");
        let component = evidence["component"]
            .as_object_mut()
            .expect("component evidence is an object");
        let env = component.remove("env").unwrap_or_default();
        let extensions = component.remove("extensions").unwrap_or_default();
        let github = component.remove("github").unwrap_or_default();
        let remote_url = component.remove("remote_url").unwrap_or_default();
        component.insert("env".to_string(), redact_map(env));
        component.insert("extensions".to_string(), redact_extensions(extensions));
        component.insert("github".to_string(), redact_github(github));
        component.insert("remote_url".to_string(), redact_remote_url(remote_url));
        canonical_json(evidence)
    }

    #[allow(dead_code)]
    pub fn evidence_digest(&self) -> String {
        digest_json(&self.evidence())
    }
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, canonical_json(value)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        value => value,
    }
}

fn digest_json(value: &serde_json::Value) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("canonical JSON serializes"))
    )
}

fn redact_map(value: serde_json::Value) -> serde_json::Value {
    let values = value.as_object().cloned().unwrap_or_default();
    let mut keys: Vec<_> = values.keys().cloned().collect();
    keys.sort();
    serde_json::json!({
        "keys": keys,
        "fingerprint": digest_json(&canonical_json(serde_json::Value::Object(values))),
    })
}

fn redact_remote_url(value: serde_json::Value) -> serde_json::Value {
    let Some(url) = value.as_str() else {
        return serde_json::Value::Null;
    };
    let (scheme, authority) = url
        .split_once("://")
        .map_or((None, None), |(scheme, remainder)| {
            (Some(scheme), remainder.split('/').next())
        });
    let host = authority.and_then(|authority| {
        authority
            .rsplit_once('@')
            .map_or(Some(authority), |(_, host)| Some(host))
            .map(|host| host.split(':').next().unwrap_or(host))
            .filter(|host| !host.is_empty())
    });
    serde_json::json!({
        "scheme": scheme,
        "host": host,
        "fingerprint": digest_json(&canonical_json(serde_json::Value::String(url.to_string()))),
    })
}

fn redact_extensions(value: serde_json::Value) -> serde_json::Value {
    let extensions = value.as_object().cloned().unwrap_or_default();
    let extensions = extensions
        .into_iter()
        .map(|(id, settings)| {
            let settings = settings.as_object().cloned().unwrap_or_default();
            (id, redact_map(serde_json::Value::Object(settings)))
        })
        .collect();
    serde_json::Value::Object(extensions)
}

fn redact_github(value: serde_json::Value) -> serde_json::Value {
    let hosts = value
        .get("hosts")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let hosts: serde_json::Map<String, serde_json::Value> = hosts
        .into_iter()
        .map(|(host, config)| {
            let config = config.as_object().cloned().unwrap_or_default();
            let proxy = config.get("proxy").cloned().unwrap_or_default();
            let env = config.get("env").cloned().unwrap_or_default();
            (
                host,
                serde_json::json!({
                    "proxy_fingerprint": digest_json(&canonical_json(proxy)),
                    "env": redact_map(env),
                }),
            )
        })
        .collect();
    serde_json::json!({ "hosts": hosts })
}

/// Deploy configuration that can affect source selection or payload preparation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreparationConfig {
    pub skip_build: bool,
    pub keep_deps: bool,
    pub skip_deps_hydration: bool,
    pub expected_version: Option<String>,
    pub no_pull: bool,
    pub allow_stale_source: bool,
    pub head: bool,
    pub requested_ref: Option<String>,
    pub tagged: bool,
    pub prepared_artifact: Option<PreparedDeployArtifact>,
}

impl From<&DeployConfig> for PreparationConfig {
    fn from(config: &DeployConfig) -> Self {
        Self {
            skip_build: config.skip_build,
            keep_deps: config.keep_deps,
            skip_deps_hydration: config.skip_deps_hydration,
            expected_version: config.expected_version.clone(),
            no_pull: config.no_pull,
            allow_stale_source: config.allow_stale_source,
            head: config.head,
            requested_ref: config.requested_ref.clone(),
            tagged: config.tagged,
            prepared_artifact: config.prepared_artifact.clone(),
        }
    }
}

/// Request-indexed payload ownership. Inserting an equal request retains the
/// original resource lease until the collection itself is dropped.
#[derive(Default)]
pub(crate) struct PreparedPayloadCollection {
    entries: HashMap<String, PreparedPayloadEntry>,
}

struct PreparedPayloadEntry {
    _request: ComponentPayloadPreparationRequest,
    _release_artifact: Option<ReleaseArtifactLease>,
}

impl PreparedPayloadCollection {
    pub fn insert(
        &mut self,
        request: ComponentPayloadPreparationRequest,
        release_artifact: Option<ReleaseArtifactLease>,
    ) -> Result<()> {
        let identity = request.identity();
        match self.entries.entry(identity) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(PreparedPayloadEntry {
                    _request: request,
                    _release_artifact: release_artifact,
                });
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                match (&existing._release_artifact, release_artifact) {
                    (None, Some(release_artifact)) => {
                        existing._release_artifact = Some(release_artifact);
                        Ok(())
                    }
                    (Some(existing), Some(candidate)) if **existing != *candidate => {
                        Err(Error::validation_invalid_argument(
                            "release_artifact",
                            "Equal payload preparation requests referenced incompatible release artifacts",
                            None,
                            None,
                        ))
                    }
                    _ => Ok(()),
                }
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::{
        Component, GithubConfig, GithubHostConfig, ScopedExtensionConfig,
    };
    use std::collections::HashMap;

    fn config() -> DeployConfig {
        DeployConfig {
            component_ids: vec!["fixture".to_string()],
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            skip_deps_hydration: false,
            expected_version: None,
            no_pull: false,
            allow_stale_source: false,
            allow_downgrade: false,
            head: false,
            requested_ref: None,
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        }
    }

    fn component() -> Component {
        Component::new(
            "fixture".to_string(),
            "/source/fixture".to_string(),
            "dist/fixture.zip".to_string(),
            None,
        )
    }

    fn sensitive_component(reverse_insertion_order: bool, secret: &str) -> Component {
        let mut component = component();
        component
            .env
            .insert("BUILD_TOKEN".to_string(), secret.to_string());

        let mut first_settings = HashMap::new();
        let mut second_settings = HashMap::new();
        let pairs: Vec<(&str, &str)> = if reverse_insertion_order {
            vec![("region", "us-east-1"), ("api_token", secret)]
        } else {
            vec![("api_token", secret), ("region", "us-east-1")]
        };
        for (key, value) in pairs {
            first_settings.insert(key.to_string(), value.to_string().into());
            second_settings.insert(key.to_string(), value.to_string().into());
        }
        let extension_pairs = [
            (
                "first".to_string(),
                ScopedExtensionConfig {
                    version: Some("1.0".to_string()),
                    settings: first_settings,
                },
            ),
            (
                "second".to_string(),
                ScopedExtensionConfig {
                    version: Some("2.0".to_string()),
                    settings: second_settings,
                },
            ),
        ];
        let extension_pairs: Vec<_> = if reverse_insertion_order {
            extension_pairs.into_iter().rev().collect()
        } else {
            extension_pairs.into_iter().collect()
        };
        let mut extensions = HashMap::new();
        for (key, value) in extension_pairs {
            extensions.insert(key, value);
        }
        component.extensions = Some(extensions);

        let mut hosts = HashMap::new();
        for host in if reverse_insertion_order {
            ["github.example.test", "github.com"]
        } else {
            ["github.com", "github.example.test"]
        } {
            let mut env = HashMap::new();
            env.insert("AUTH_TOKEN".to_string(), secret.to_string());
            hosts.insert(
                host.to_string(),
                GithubHostConfig {
                    proxy: Some(format!("https://{secret}@proxy.test")),
                    env,
                },
            );
        }
        component.github = GithubConfig { hosts };
        component
    }

    #[test]
    fn binding_only_overrides_share_a_preparation_request() {
        let source = component();
        let config = config();
        let first_project = crate::core::project::Project {
            id: "first".to_string(),
            component_overrides: [(
                "fixture".to_string(),
                crate::core::project::ProjectComponentOverrides {
                    remote_path: Some("plugins/one".to_string()),
                    hooks: [("post:deploy".to_string(), vec!["one".to_string()])]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let second_project = crate::core::project::Project {
            id: "second".to_string(),
            component_overrides: [(
                "fixture".to_string(),
                crate::core::project::ProjectComponentOverrides {
                    remote_path: Some("plugins/two".to_string()),
                    deploy_strategy: Some("git".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        let (first_preparation, first_binding) =
            crate::core::project::component::overrides::resolve_deploy_override_inputs(
                &source,
                &first_project,
            );
        let (second_preparation, second_binding) =
            crate::core::project::component::overrides::resolve_deploy_override_inputs(
                &source,
                &second_project,
            );

        let first = ComponentPayloadPreparationRequest::new(&first_preparation, &config);
        let second = ComponentPayloadPreparationRequest::new(&second_preparation, &config);
        assert_eq!(first.identity(), second.identity());
        assert!(first.evidence()["component"].get("remote_path").is_none());
        assert_ne!(first_binding.remote_path, second_binding.remote_path);
        assert_ne!(
            first_binding.deploy_strategy,
            second_binding.deploy_strategy
        );

        let mut collection = PreparedPayloadCollection::default();
        collection.insert(first, None).expect("first request");
        collection.insert(second, None).expect("equal request");
        assert_eq!(collection.len(), 1);
    }

    #[test]
    fn build_artifact_override_changes_preparation_identity() {
        let source = component();
        let config = config();
        let project = crate::core::project::Project {
            id: "custom-build".to_string(),
            component_overrides: [(
                "fixture".to_string(),
                crate::core::project::ProjectComponentOverrides {
                    build_artifact: Some("release/fixture.zip".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let (prepared, _) =
            crate::core::project::component::overrides::resolve_deploy_override_inputs(
                &source, &project,
            );

        assert_ne!(
            ComponentPayloadPreparationRequest::new(&source, &config).identity(),
            ComponentPayloadPreparationRequest::new(&prepared, &config).identity()
        );
    }

    #[test]
    fn requests_are_pure_in_memory_transforms() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("missing-source");
        let artifact_path = temp.path().join("missing-artifact.zip");
        let mut component = component();
        component.local_path = source_path.display().to_string();
        component.build_artifact = Some(artifact_path.display().to_string());
        let entries_before = std::fs::read_dir(temp.path())
            .expect("read temp dir")
            .count();
        let request = ComponentPayloadPreparationRequest::new(&component, &config());
        let mut collection = PreparedPayloadCollection::default();
        collection.insert(request, None).expect("pure insert");

        assert_eq!(collection.len(), 1);
        assert!(!source_path.exists());
        assert!(!artifact_path.exists());
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("read temp dir")
                .count(),
            entries_before
        );
    }

    #[test]
    fn collection_retains_release_artifact_lease_after_caller_drops_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact_path = temp.path().join("fixture.zip");
        std::fs::write(&artifact_path, "payload").expect("artifact");
        let lease = super::super::release_download::ReleaseArtifactLease::test_new(
            super::super::release_download::ReleaseArtifact {
                path: artifact_path,
                tag: "v1.0.0".to_string(),
                commit: None,
                url: "https://example.test/fixture.zip".to_string(),
                name: "fixture.zip".to_string(),
                size: 7,
                sha256: "hash".to_string(),
            },
        )
        .expect("lease");
        let observer = lease.clone();
        let mut collection = PreparedPayloadCollection::default();
        collection
            .insert(
                ComponentPayloadPreparationRequest::new(&component(), &config()),
                Some(lease),
            )
            .expect("lease insert");

        assert_eq!(observer.test_strong_count(), 2);
        drop(collection);
        assert_eq!(observer.test_strong_count(), 1);
    }

    #[test]
    fn canonical_identity_and_evidence_ignore_map_insertion_order() {
        let first = ComponentPayloadPreparationRequest::new(
            &sensitive_component(false, "sentinel-secret"),
            &config(),
        );
        let second = ComponentPayloadPreparationRequest::new(
            &sensitive_component(true, "sentinel-secret"),
            &config(),
        );

        assert_eq!(first.identity(), second.identity());
        assert_eq!(first.evidence(), second.evidence());
        assert_eq!(first.evidence_digest(), second.evidence_digest());
        assert_ne!(
            first.identity(),
            ComponentPayloadPreparationRequest::new(
                &sensitive_component(true, "changed-secret"),
                &config(),
            )
            .identity()
        );
    }

    #[test]
    fn evidence_redacts_sensitive_preparation_values() {
        let request = ComponentPayloadPreparationRequest::new(
            &sensitive_component(false, "sentinel-secret"),
            &config(),
        );
        let evidence = serde_json::to_string(&request.evidence()).expect("evidence JSON");

        assert!(!evidence.contains("sentinel-secret"));
        assert!(!evidence.contains("https://"));
        assert!(evidence.contains("BUILD_TOKEN"));
        assert!(evidence.contains("api_token"));
        assert!(evidence.contains("AUTH_TOKEN"));
    }

    #[test]
    fn evidence_redacts_authenticated_remote_urls() {
        let mut component = component();
        component.remote_url =
            Some("https://username:sentinel-token@example.test/org/repo.git".to_string());
        let request = ComponentPayloadPreparationRequest::new(&component, &config());
        let evidence = serde_json::to_string(&request.evidence()).expect("evidence JSON");
        assert!(!evidence.contains("username"));
        assert!(!evidence.contains("sentinel-token"));
        assert!(evidence.contains("example.test"));
        assert_ne!(
            request.identity(),
            ComponentPayloadPreparationRequest::new(
                &Component {
                    remote_url: Some(
                        "https://username:changed-token@example.test/org/repo.git".to_string()
                    ),
                    ..component
                },
                &config(),
            )
            .identity()
        );
    }

    #[test]
    fn target_safety_overrides_do_not_change_preparation_identity() {
        let component = component();
        let base = config();
        let mut target_policy = config();
        target_policy.force = true;
        target_policy.allow_downgrade = true;

        assert_eq!(
            ComponentPayloadPreparationRequest::new(&component, &base).identity(),
            ComponentPayloadPreparationRequest::new(&component, &target_policy).identity()
        );
    }

    #[test]
    fn collection_merges_late_lease_and_rejects_conflicts() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact_path = temp.path().join("fixture.zip");
        std::fs::write(&artifact_path, "payload").expect("artifact");
        let lease = super::super::release_download::ReleaseArtifactLease::test_new(
            super::super::release_download::ReleaseArtifact {
                path: artifact_path,
                tag: "v1.0.0".to_string(),
                commit: None,
                url: "https://example.test/fixture.zip".to_string(),
                name: "fixture.zip".to_string(),
                size: 7,
                sha256: "hash".to_string(),
            },
        )
        .expect("lease");
        let observer = lease.clone();
        let request = ComponentPayloadPreparationRequest::new(&component(), &config());
        let mut collection = PreparedPayloadCollection::default();
        collection.insert(request.clone(), None).expect("no lease");
        collection
            .insert(request.clone(), Some(lease.clone()))
            .expect("late lease retained");
        collection
            .insert(request.clone(), Some(lease))
            .expect("identical lease deduplicates");
        assert_eq!(observer.test_strong_count(), 2);

        let other_temp = tempfile::tempdir().expect("other temp dir");
        let other_path = other_temp.path().join("fixture.zip");
        std::fs::write(&other_path, "changed").expect("other artifact");
        let conflicting = super::super::release_download::ReleaseArtifactLease::test_new(
            super::super::release_download::ReleaseArtifact {
                path: other_path,
                tag: "v1.0.1".to_string(),
                commit: Some("different".to_string()),
                url: "https://example.test/other.zip".to_string(),
                name: "fixture.zip".to_string(),
                size: 7,
                sha256: "different".to_string(),
            },
        )
        .expect("conflicting lease");
        assert!(collection.insert(request, Some(conflicting)).is_err());
    }
}
