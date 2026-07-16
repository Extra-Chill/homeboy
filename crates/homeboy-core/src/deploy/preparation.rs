use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::component::Component;
use crate::error::{Error, Result};

use super::execution::{
    release_artifact_plan, resolve_planned_release_artifact, ReleaseArtifactPlan,
};
use super::orchestration_ref_checkout::{ExactRefCheckout, ExactRefIdentity};
use super::{sha256_file, DeployConfig, PreparedDeployArtifact};
use crate::git::release_download::{ReleaseArtifactLease, ReleaseArtifactStore};

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
        Option<std::collections::HashMap<String, crate::component::ScopedExtensionConfig>>,
    pub capability_extensions: std::collections::HashMap<String, String>,
    pub scopes: Option<crate::component::ScopeConfig>,
    pub artifact_inputs: Vec<crate::component::ArtifactInput>,
    pub scripts: Option<crate::component::ComponentScriptsConfig>,
    pub version_targets: Option<Vec<crate::component::VersionTarget>>,
    pub env: std::collections::BTreeMap<String, String>,
    pub remote_url: Option<String>,
    pub github: crate::component::GithubConfig,
    pub release: crate::component::ComponentReleaseConfig,
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
                version_targets: component.version_targets.clone(),
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

    fn component(&self) -> Component {
        Component {
            id: self.component.id.clone(),
            local_path: self.component.local_path.clone(),
            build_artifact: self.component.build_artifact.clone(),
            build_command: self.component.build_command.clone(),
            extensions: self.component.extensions.clone(),
            capability_extensions: self.component.capability_extensions.clone(),
            scopes: self.component.scopes.clone(),
            artifact_inputs: self.component.artifact_inputs.clone(),
            scripts: self.component.scripts.clone(),
            version_targets: self.component.version_targets.clone(),
            env: self.component.env.clone(),
            remote_url: self.component.remote_url.clone(),
            github: self.component.github.clone(),
            release: self.component.release.clone(),
            ..Component::default()
        }
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

/// Immutable local payload prepared without project or target inputs.
///
/// Orchestration invokes this after resolving project and SSH context, but before
/// any target mutation such as transfer, install, hooks, or smoke checks.
pub(crate) struct PreparedComponentPayload {
    pub artifact: PreparedDeployArtifact,
    pub source_commit: String,
    pub exact_ref: Option<ExactRefIdentity>,
    _release_artifact: Option<ReleaseArtifactLease>,
    _exact_ref_checkout: Option<ExactRefCheckout>,
    payload_artifact: PreparedArtifactCleanup,
}

/// Owns only the temporary copy made by preparation. The configured component
/// artifact is caller-owned and must survive both successful and failed deploys.
struct PreparedArtifactCleanup(Option<PathBuf>);

impl Drop for PreparedArtifactCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Cleans only source output that did not exist when this preparation began.
/// It remains armed through every post-build validation and copy operation.
struct GeneratedSourceArtifactCleanup {
    artifact: PathBuf,
    artifact_existed: bool,
    artifact_parent: Option<PathBuf>,
    artifact_parent_existed: bool,
    build_dir: PathBuf,
    build_dir_existed: bool,
    armed: bool,
}

impl GeneratedSourceArtifactCleanup {
    fn new(component: &Component) -> Result<Self> {
        let artifact = artifact_path(component)?;
        let build_dir =
            Path::new(&component.local_path).join(crate::defaults::deploy_generated_build_dir());
        let artifact_parent = artifact.parent().map(Path::to_path_buf);
        Ok(Self {
            artifact_existed: artifact.exists(),
            artifact_parent_existed: artifact_parent.as_ref().is_some_and(|path| path.exists()),
            artifact_parent,
            build_dir_existed: build_dir.exists(),
            artifact,
            build_dir,
            armed: true,
        })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GeneratedSourceArtifactCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if !self.artifact_existed {
            let _ = std::fs::remove_file(&self.artifact);
            if !self.artifact_parent_existed {
                if let Some(parent) = self.artifact_parent.as_ref() {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
        if !self.build_dir_existed {
            let _ = std::fs::remove_dir_all(&self.build_dir);
        }
    }
}

/// Request-indexed payload ownership. Equal requests share one prepared payload
/// and its RAII resources until this collection is dropped.
#[derive(Default)]
pub(crate) struct PreparedPayloadCollection {
    entries: HashMap<String, PreparedPayloadEntry>,
}

struct PreparedPayloadEntry {
    request: ComponentPayloadPreparationRequest,
    payload: Option<PreparedComponentPayload>,
    release_artifact: Option<ReleaseArtifactLease>,
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
                    request,
                    payload: None,
                    release_artifact,
                });
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                if let Some(payload) = existing.payload.as_ref() {
                    return match (&payload._release_artifact, release_artifact) {
                        (Some(existing), Some(candidate)) if **existing == *candidate => Ok(()),
                        (_, None) => Ok(()),
                        _ => Err(Error::validation_invalid_argument(
                            "release_artifact",
                            "Prepared payload cannot be rebound to a different release artifact",
                            None,
                            None,
                        )),
                    };
                }
                match (&existing.release_artifact, release_artifact) {
                    (None, Some(release_artifact)) => {
                        existing.release_artifact = Some(release_artifact);
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

    /// Prepare a component once and return its immutable artifact identity. This
    /// boundary intentionally has no project or remote target inputs.
    pub fn prepare(
        &mut self,
        request: ComponentPayloadPreparationRequest,
        release_artifacts: &mut ReleaseArtifactStore,
    ) -> Result<&PreparedComponentPayload> {
        let identity = request.identity();
        if let Some(existing) = self.entries.get_mut(&identity) {
            if existing.payload.is_none() {
                let release_artifact = existing.release_artifact.take();
                existing.payload = Some(prepare_payload(
                    &existing.request,
                    release_artifacts,
                    release_artifact,
                )?);
            }
        } else {
            let payload = prepare_payload(&request, release_artifacts, None)?;
            self.entries.insert(
                identity.clone(),
                PreparedPayloadEntry {
                    request,
                    payload: Some(payload),
                    release_artifact: None,
                },
            );
        }
        Ok(self.entries[&identity]
            .payload
            .as_ref()
            .expect("prepared payload is populated exactly once"))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

fn prepare_payload(
    request: &ComponentPayloadPreparationRequest,
    release_artifacts: &mut ReleaseArtifactStore,
    retained_release_artifact: Option<ReleaseArtifactLease>,
) -> Result<PreparedComponentPayload> {
    if let Some(artifact) = request.config.prepared_artifact.clone() {
        artifact.validate(
            &request.component.id,
            request.config.expected_version.as_deref(),
        )?;
        return Ok(PreparedComponentPayload {
            source_commit: artifact.source_commit.clone(),
            artifact,
            exact_ref: None,
            _release_artifact: None,
            _exact_ref_checkout: None,
            payload_artifact: PreparedArtifactCleanup(None),
        });
    }

    let component = request.component();
    let checkout = request
        .config
        .requested_ref
        .as_deref()
        .map(|reference| ExactRefCheckout::materialize(&component, reference))
        .transpose()?;
    if let Some(checkout) = checkout.as_ref() {
        checkout.verify()?;
        checkout.hydrate_dependencies(request.config.skip_deps_hydration)?;
    }
    let component = checkout
        .as_ref()
        .map(|checkout| checkout.component.clone())
        .unwrap_or(component);
    let exact_ref = checkout.as_ref().map(|checkout| checkout.identity.clone());

    let release_artifact = match release_artifact_plan(
        &component,
        &DeployConfig::from_preparation(request),
        false,
        false,
    ) {
        ReleaseArtifactPlan::Reuse { tag, .. } => Some(match retained_release_artifact {
            Some(artifact) => artifact,
            None => resolve_planned_release_artifact(&component, &tag, release_artifacts).map_err(
                |message| {
                    Error::validation_invalid_argument("releaseArtifact", message, None, None)
                },
            )?,
        }),
        ReleaseArtifactPlan::LocalBuild { .. } => None,
    };
    let mut generated_source_artifact = None;
    if release_artifact.is_none() && !request.config.skip_build {
        let cleanup = GeneratedSourceArtifactCleanup::new(&component)?;
        let (build_exit_code, build_error) = crate::build::build_component(&component);
        if let Some(message) = build_error {
            return Err(Error::validation_invalid_argument(
                "build",
                format!(
                    "Build failed (exit code {}): {message}",
                    build_exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string())
                ),
                None,
                None,
            ));
        }
        generated_source_artifact = Some(cleanup);
    }
    let path = release_artifact
        .as_ref()
        .map(|artifact| artifact.path.clone())
        .unwrap_or(artifact_path(&component)?);
    let metadata = std::fs::metadata(&path).map_err(|error| {
        Error::internal_io(
            format!("Prepared artifact is unavailable: {error}"),
            Some(path.display().to_string()),
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(Error::validation_invalid_argument(
            "artifact",
            "Prepared artifact must be a non-empty file",
            Some(path.display().to_string()),
            None,
        ));
    }
    let digest = sha256_file(&path)?;
    if let Some(release_artifact) = release_artifact.as_ref() {
        if metadata.len() != release_artifact.size || digest != release_artifact.sha256 {
            return Err(Error::validation_invalid_argument(
                "releaseArtifact",
                "Retained release artifact does not match its verified size or SHA-256",
                Some(path.display().to_string()),
                None,
            ));
        }
    }
    let version = crate::release::version::get_component_version(&component).unwrap_or_default();
    if request
        .config
        .expected_version
        .as_deref()
        .is_some_and(|expected| expected != version)
    {
        return Err(Error::validation_invalid_argument(
            "version",
            "Prepared artifact source version does not match expected version",
            None,
            None,
        ));
    }
    let source_commit = exact_ref
        .as_ref()
        .map(|identity| identity.resolved_sha.clone())
        .or_else(|| {
            crate::engine::command::run_in_optional(
                &component.local_path,
                "git",
                &["rev-parse", "HEAD"],
            )
        })
        .unwrap_or_default();
    let (durable_path, payload_artifact) = if release_artifact.is_some() {
        (path.clone(), PreparedArtifactCleanup(None))
    } else {
        copy_prepared_artifact(&path)?
    };
    let artifact = PreparedDeployArtifact {
        component_id: component.id.clone(),
        path: durable_path.display().to_string(),
        durable_path: durable_path.display().to_string(),
        size_bytes: metadata.len(),
        sha256: digest,
        version: version.clone(),
        tag: exact_ref
            .as_ref()
            .map(|identity| identity.requested_ref.clone())
            .unwrap_or_else(|| format!("v{version}")),
        source_commit: source_commit.clone(),
    };
    artifact.validate(&component.id, request.config.expected_version.as_deref())?;
    if let Some(cleanup) = generated_source_artifact.as_mut() {
        cleanup.disarm();
    }
    Ok(PreparedComponentPayload {
        artifact,
        source_commit,
        exact_ref,
        _release_artifact: release_artifact,
        _exact_ref_checkout: checkout,
        payload_artifact,
    })
}

fn copy_prepared_artifact(source: &Path) -> Result<(PathBuf, PreparedArtifactCleanup)> {
    let directory = std::env::temp_dir()
        .join("homeboy-deploy-payload")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).map_err(|error| {
        Error::internal_io(
            format!("Failed to create prepared artifact directory: {error}"),
            Some(directory.display().to_string()),
        )
    })?;
    let cleanup = PreparedArtifactCleanup(Some(directory.clone()));
    let filename = source.file_name().ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact",
            "Prepared artifact path has no file name",
            Some(source.display().to_string()),
            None,
        )
    })?;
    let destination = directory.join(filename);
    std::fs::copy(source, &destination).map_err(|error| {
        Error::internal_io(
            format!("Failed to copy prepared artifact: {error}"),
            Some(source.display().to_string()),
        )
    })?;
    Ok((destination, cleanup))
}

fn artifact_path(component: &Component) -> Result<PathBuf> {
    component
        .build_artifact
        .as_ref()
        .map(|artifact| {
            let path = Path::new(artifact);
            Ok(if path.is_absolute() {
                path.to_path_buf()
            } else {
                Path::new(&component.local_path).join(path)
            })
        })
        .transpose()?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "build_artifact",
                "Component has no build artifact to prepare",
                None,
                None,
            )
        })
}

impl DeployConfig {
    fn from_preparation(request: &ComponentPayloadPreparationRequest) -> Self {
        Self {
            component_ids: vec![request.component.id.clone()],
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: request.config.skip_build,
            keep_deps: request.config.keep_deps,
            skip_deps_hydration: request.config.skip_deps_hydration,
            expected_version: request.config.expected_version.clone(),
            no_pull: request.config.no_pull,
            allow_stale_source: request.config.allow_stale_source,
            allow_downgrade: false,
            head: request.config.head,
            requested_ref: request.config.requested_ref.clone(),
            tagged: request.config.tagged,
            prepared_artifact: None,
            resume_run_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, GithubConfig, GithubHostConfig, ScopedExtensionConfig};
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
        let first_project = crate::project::Project {
            id: "first".to_string(),
            component_overrides: [(
                "fixture".to_string(),
                crate::project::ProjectComponentOverrides {
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
        let second_project = crate::project::Project {
            id: "second".to_string(),
            component_overrides: [(
                "fixture".to_string(),
                crate::project::ProjectComponentOverrides {
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
            crate::project::component::overrides::resolve_deploy_override_inputs(
                &source,
                &first_project,
            );
        let (second_preparation, second_binding) =
            crate::project::component::overrides::resolve_deploy_override_inputs(
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
        let project = crate::project::Project {
            id: "custom-build".to_string(),
            component_overrides: [(
                "fixture".to_string(),
                crate::project::ProjectComponentOverrides {
                    build_artifact: Some("release/fixture.zip".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let (prepared, _) =
            crate::project::component::overrides::resolve_deploy_override_inputs(&source, &project);

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
        let lease = crate::git::release_download::ReleaseArtifactLease::test_new(
            crate::git::release_download::ReleaseArtifact {
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
        let lease = crate::git::release_download::ReleaseArtifactLease::test_new(
            crate::git::release_download::ReleaseArtifact {
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
        let conflicting = crate::git::release_download::ReleaseArtifactLease::test_new(
            crate::git::release_download::ReleaseArtifact {
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

    #[test]
    fn equal_inserted_request_builds_once_and_cleans_only_its_payload_copy() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_artifact = temp.path().join("build/fixture.zip");
        let build_count = temp.path().join("build-count");
        let mut component = component();
        component.local_path = temp.path().display().to_string();
        component.build_artifact = Some("build/fixture.zip".to_string());
        component.scripts = Some(crate::component::ComponentScriptsConfig {
            build: vec![format!(
                "mkdir -p build && printf payload > build/fixture.zip && printf build >> {}",
                build_count.display()
            )],
            ..Default::default()
        });
        let request = ComponentPayloadPreparationRequest::new(&component, &config());
        let mut collection = PreparedPayloadCollection::default();
        collection
            .insert(request.clone(), None)
            .expect("insert request");

        let payload_path = collection
            .prepare(request.clone(), &mut ReleaseArtifactStore::default())
            .expect("prepare inserted request")
            .artifact
            .effective_path()
            .to_string();
        collection
            .prepare(request, &mut ReleaseArtifactStore::default())
            .expect("reuse prepared payload");

        assert_eq!(
            std::fs::read_to_string(&build_count).expect("build count"),
            "build"
        );
        assert!(
            source_artifact.exists(),
            "source artifact remains caller-owned"
        );
        assert_ne!(Path::new(&payload_path), source_artifact);
        assert!(
            Path::new(&payload_path).exists(),
            "payload copy is retained"
        );

        drop(collection);
        assert!(
            source_artifact.exists(),
            "dropping payload never removes source artifact"
        );
        assert!(
            !Path::new(&payload_path).exists(),
            "dropping payload removes its owned copy"
        );
    }

    #[test]
    fn inserted_release_lease_is_consumed_by_equal_preparation_request() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact_path = temp.path().join("fixture.zip");
        std::fs::write(&artifact_path, "payload").expect("artifact");
        std::fs::write(temp.path().join("package.json"), r#"{"version":"1.0.0"}"#)
            .expect("package manifest");
        let lease = ReleaseArtifactLease::test_new(crate::git::release_download::ReleaseArtifact {
            path: artifact_path.clone(),
            tag: "v1.0.0".to_string(),
            commit: Some("0123456789abcdef".to_string()),
            url: "https://example.test/fixture.zip".to_string(),
            name: "fixture.zip".to_string(),
            size: 7,
            sha256: sha256_file(&artifact_path).expect("sha"),
        })
        .expect("lease");
        let observer = lease.clone();
        let mut component = component();
        component.local_path = temp.path().display().to_string();
        component.build_artifact = Some("fixture.zip".to_string());
        component.remote_url = Some("https://github.com/example/fixture".to_string());
        component.version_targets = Some(vec![crate::component::VersionTarget {
            file: "package.json".to_string(),
            pattern: Some(r#""version"\s*:\s*"([^"]+)""#.to_string()),
            artifact_path: None,
        }]);
        let mut deploy_config = config();
        deploy_config.expected_version = Some("1.0.0".to_string());
        let request = ComponentPayloadPreparationRequest::new(&component, &deploy_config);
        let mut collection = PreparedPayloadCollection::default();
        collection
            .insert(request.clone(), Some(lease))
            .expect("insert lease");

        let payload = collection
            .prepare(request, &mut ReleaseArtifactStore::default())
            .expect("prepare retained lease");
        assert_eq!(Path::new(payload.artifact.effective_path()), artifact_path);
        assert_eq!(observer.test_strong_count(), 2, "payload retains the lease");

        collection
            .insert(
                ComponentPayloadPreparationRequest::new(&component, &deploy_config),
                Some(observer.clone()),
            )
            .expect("equal consumer reuses the verified release artifact");
        let conflicting_temp = tempfile::tempdir().expect("conflicting temp dir");
        let conflicting_path = conflicting_temp.path().join("conflicting.zip");
        std::fs::write(&conflicting_path, "changed").expect("conflicting artifact");
        let conflicting =
            ReleaseArtifactLease::test_new(crate::git::release_download::ReleaseArtifact {
                path: conflicting_path,
                tag: "v1.0.0".to_string(),
                commit: Some("fedcba9876543210".to_string()),
                url: "https://example.test/conflicting.zip".to_string(),
                name: "fixture.zip".to_string(),
                size: 7,
                sha256: "different".to_string(),
            })
            .expect("conflicting lease");
        assert!(collection
            .insert(
                ComponentPayloadPreparationRequest::new(&component, &deploy_config),
                Some(conflicting),
            )
            .is_err());
        assert_eq!(
            observer.test_strong_count(),
            2,
            "equal consumers do not retain a redundant lease"
        );
        drop(collection);
        assert_eq!(
            observer.test_strong_count(),
            1,
            "payload releases its lease"
        );
    }

    #[test]
    fn retained_release_mismatch_creates_no_payload_or_local_build_effect() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact_path = temp.path().join("fixture.zip");
        let build_counter = temp.path().join("build-count");
        std::fs::write(&artifact_path, "payload").expect("artifact");
        let lease = ReleaseArtifactLease::test_new(crate::git::release_download::ReleaseArtifact {
            path: artifact_path,
            tag: "v1.0.0".to_string(),
            commit: None,
            url: "https://example.test/fixture.zip".to_string(),
            name: "fixture.zip".to_string(),
            size: 7,
            sha256: "wrong".to_string(),
        })
        .expect("lease");
        let mut component = component();
        component.local_path = temp.path().display().to_string();
        component.build_artifact = Some("fixture.zip".to_string());
        component.remote_url = Some("https://github.com/example/fixture".to_string());
        component.scripts = Some(crate::component::ComponentScriptsConfig {
            build: vec![format!("printf build > {}", build_counter.display())],
            ..Default::default()
        });
        let mut deploy_config = config();
        deploy_config.expected_version = Some("1.0.0".to_string());
        let request = ComponentPayloadPreparationRequest::new(&component, &deploy_config);
        let mut collection = PreparedPayloadCollection::default();
        collection
            .insert(request.clone(), Some(lease))
            .expect("insert lease");

        let error = match collection.prepare(request, &mut ReleaseArtifactStore::default()) {
            Ok(_) => panic!("mismatched retained release must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("does not match"));
        assert_eq!(collection.len(), 1, "request is retained without a payload");
        assert!(
            !build_counter.exists(),
            "release mismatch must not build or target"
        );
    }

    #[test]
    fn failed_post_build_validation_removes_only_new_source_output() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("build/fixture.zip");
        let mut component = component();
        component.local_path = temp.path().display().to_string();
        component.build_artifact = Some("build/fixture.zip".to_string());
        component.scripts = Some(crate::component::ComponentScriptsConfig {
            build: vec!["mkdir -p build && printf payload > build/fixture.zip".to_string()],
            ..Default::default()
        });
        let mut deploy_config = config();
        deploy_config.expected_version = Some("9.9.9".to_string());
        let request = ComponentPayloadPreparationRequest::new(&component, &deploy_config);

        let error = match PreparedPayloadCollection::default()
            .prepare(request, &mut ReleaseArtifactStore::default())
        {
            Ok(_) => panic!("version validation must fail after build"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("source version"));
        assert!(
            !artifact.exists(),
            "new source artifact is cleaned after validation failure"
        );
    }

    #[test]
    fn exact_ref_preparation_builds_detached_bytes_and_cleans_owned_resources() {
        fn git(path: &Path, args: &[&str]) -> String {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?}: {:?}", args, output);
            String::from_utf8(output.stdout)
                .expect("git output")
                .trim()
                .to_string()
        }

        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        std::fs::write(repo.path().join("payload.txt"), "accepted\n").expect("accepted payload");
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-q", "-m", "accepted"]);
        git(repo.path(), &["branch", "accepted"]);
        let accepted_sha = git(repo.path(), &["rev-parse", "accepted"]);
        std::fs::write(repo.path().join("payload.txt"), "configured\n")
            .expect("configured payload");
        git(repo.path(), &["commit", "-am", "configured", "-q"]);
        let configured_head = git(repo.path(), &["rev-parse", "HEAD"]);
        let mut component = component();
        component.local_path = repo.path().display().to_string();
        component.build_artifact = Some("build/fixture.zip".to_string());
        component.scripts = Some(crate::component::ComponentScriptsConfig {
            build: vec![
                "mkdir -p build && git show HEAD:payload.txt > build/fixture.zip".to_string(),
            ],
            ..Default::default()
        });
        let mut deploy_config = config();
        deploy_config.requested_ref = Some("accepted".to_string());
        deploy_config.skip_deps_hydration = true;
        let request = ComponentPayloadPreparationRequest::new(&component, &deploy_config);
        let mut collection = PreparedPayloadCollection::default();
        let payload_path = collection
            .prepare(request, &mut ReleaseArtifactStore::default())
            .expect("prepare exact ref")
            .artifact
            .effective_path()
            .to_string();

        assert_eq!(
            std::fs::read_to_string(&payload_path).expect("payload bytes"),
            "accepted\n"
        );
        assert_eq!(
            collection
                .entries
                .values()
                .next()
                .unwrap()
                .payload
                .as_ref()
                .unwrap()
                .source_commit,
            accepted_sha
        );
        assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), configured_head);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("payload.txt")).expect("configured content"),
            "configured\n"
        );
        drop(collection);
        assert!(
            !Path::new(&payload_path).exists(),
            "payload copy is cleaned"
        );
        assert_eq!(
            git(repo.path(), &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }
}
