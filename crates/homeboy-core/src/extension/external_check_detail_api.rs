//! Typed Extension API inventory and hydration for external-check detail resolvers.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest,
    ExtensionApiExternalCheckDetailDiagnostic, ExtensionApiExternalCheckDetailDiagnosticKind,
    ExtensionApiExternalCheckDetailHydrateRequest, ExtensionApiExternalCheckDetailHydrateResponse,
    ExtensionApiExternalCheckDetailInventoryEntry, ExtensionApiExternalCheckDetailInventoryRequest,
    ExtensionApiExternalCheckDetailInventoryResponse,
    ExtensionApiExternalCheckDetailProviderValidation, ExtensionApiOperationFailure,
    EXTENSION_API_CATALOG_REQUEST_SCHEMA,
    EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA,
    EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_RESPONSE_SCHEMA,
    EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_REQUEST_SCHEMA,
    EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_RESPONSE_SCHEMA, EXTENSION_API_V1,
    EXTERNAL_CHECK_DETAIL_RESOLVER_CAPABILITY_PREFIX,
};
use homeboy_extension_contract::{
    ExternalCheckDetailRequest, ExternalCheckDetailResolverConfig,
    ExternalCheckDetailResolverDeclaration, ExternalCheckDetailResponse,
    EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA, EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA,
};

use crate::extension::catalog::{snapshot_api, validate_operation_request};
use crate::extension::invoke::deadline_process::execute_deadline_process;
use crate::redaction::RedactionPolicy;

const CAPTURE_LIMIT_BYTES: usize = 64 * 1024;
const CLEANUP_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
struct ResolverCandidate {
    entry: ExtensionApiExternalCheckDetailInventoryEntry,
    config: Option<ExternalCheckDetailResolverConfig>,
    extension_path: Option<String>,
}

pub struct ExternalCheckDetailResolverApi {
    candidates: Vec<ResolverCandidate>,
    failure: Option<ExtensionApiOperationFailure>,
}

pub struct ExternalCheckDetailHydrationContext<'a> {
    pub deadline: Instant,
    pub resolve_environment: &'a dyn Fn(&str) -> Option<String>,
}

impl ExternalCheckDetailResolverApi {
    pub fn discover(request: &ExtensionApiExternalCheckDetailInventoryRequest) -> Self {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return Self {
                candidates: Vec::new(),
                failure: Some(failure),
            };
        }
        match resolver_candidates(request.api_version) {
            Ok(candidates) => Self {
                candidates,
                failure: None,
            },
            Err(failure) => Self {
                candidates: Vec::new(),
                failure: Some(failure),
            },
        }
    }

    pub fn inventory_api(&self) -> ExtensionApiExternalCheckDetailInventoryResponse {
        ExtensionApiExternalCheckDetailInventoryResponse {
            schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            providers: self
                .candidates
                .iter()
                .map(|candidate| candidate.entry.clone())
                .collect(),
            failure: self.failure.clone(),
        }
    }

    pub fn hydrate_api(
        &self,
        request: &ExtensionApiExternalCheckDetailHydrateRequest,
        context: ExternalCheckDetailHydrationContext<'_>,
    ) -> ExtensionApiExternalCheckDetailHydrateResponse {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return hydrate_failure(failure);
        }
        if let Some(failure) = self.failure.clone() {
            return hydrate_failure(failure);
        }

        let matches = self
            .candidates
            .iter()
            .filter(|candidate| candidate.entry.provider.as_deref() == Some(&request.provider))
            .collect::<Vec<_>>();
        let candidate = match matches.as_slice() {
            [] => {
                return hydrate_diagnostic(
                    &request.provider,
                    ExtensionApiExternalCheckDetailDiagnosticKind::Unknown,
                    "No installed extension declares this provider. Install or enable the extension that owns this provider, then rerun CI triage.",
                );
            }
            [candidate]
                if candidate.entry.validation
                    != ExtensionApiExternalCheckDetailProviderValidation::Valid =>
            {
                return hydrate_diagnostic(
                    &request.provider,
                    ExtensionApiExternalCheckDetailDiagnosticKind::Malformed,
                    candidate
                        .entry
                        .diagnostic
                        .as_deref()
                        .unwrap_or("The installed resolver declaration is invalid."),
                );
            }
            [candidate] => *candidate,
            _ => {
                return hydrate_diagnostic(
                    &request.provider,
                    ExtensionApiExternalCheckDetailDiagnosticKind::Ambiguous,
                    "Multiple extensions declare this exact provider; no resolver was invoked.",
                );
            }
        };
        let Some(config) = candidate.config.as_ref() else {
            return hydrate_diagnostic(
                &request.provider,
                ExtensionApiExternalCheckDetailDiagnosticKind::Malformed,
                "The installed resolver declaration is invalid.",
            );
        };
        let Some(extension_path) = candidate.extension_path.as_deref() else {
            return hydrate_diagnostic(
                &request.provider,
                ExtensionApiExternalCheckDetailDiagnosticKind::Malformed,
                "The declaring extension has no installation path for its resolver.",
            );
        };
        let (extension_root, program) = match resolve_program(Path::new(extension_path), config) {
            Ok(paths) => paths,
            Err(message) => {
                let kind = if message.starts_with("program cannot be resolved") {
                    ExtensionApiExternalCheckDetailDiagnosticKind::Unavailable
                } else {
                    ExtensionApiExternalCheckDetailDiagnosticKind::Malformed
                };
                return hydrate_diagnostic(
                    &request.provider,
                    kind,
                    &format!(
                        "Extension {} resolver program is invalid: {message}",
                        candidate.entry.owning_extension
                    ),
                );
            }
        };

        let projected_environment = config
            .public_env
            .iter()
            .chain(&config.secret_env)
            .filter_map(|name| {
                (context.resolve_environment)(name).map(|value| (name.clone(), value))
            })
            .collect::<Vec<_>>();
        let secrets = projected_environment
            .iter()
            .filter(|(name, _)| config.secret_env.contains(name))
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let provider_request = ExternalCheckDetailRequest {
            schema: EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA.to_string(),
            provider: request.provider.clone(),
            status: redact(&request.status, &secrets, 512),
            target_url: request.target_url.as_deref().map(normalize_target_url),
        };
        let payload = match serde_json::to_vec(&provider_request) {
            Ok(payload) => payload,
            Err(error) => {
                return hydrate_diagnostic(
                    &request.provider,
                    ExtensionApiExternalCheckDetailDiagnosticKind::Unavailable,
                    &format!("Resolver request serialization failed: {error}"),
                );
            }
        };
        let mut command = Command::new(program);
        command
            .args(&config.command[1..])
            .current_dir(extension_root)
            .env_clear();
        for (name, value) in projected_environment {
            command.env(name, value);
        }
        let output = match execute_deadline_process(
            command,
            &payload,
            context.deadline,
            CLEANUP_BUDGET,
            CAPTURE_LIMIT_BYTES,
            "Resolver",
        ) {
            Ok(output) => output,
            Err(error) => {
                return hydrate_diagnostic(
                    &request.provider,
                    ExtensionApiExternalCheckDetailDiagnosticKind::Unavailable,
                    &redact(&error.message, &secrets, 2048),
                );
            }
        };
        if !output.status.success() {
            return hydrate_diagnostic(
                &request.provider,
                ExtensionApiExternalCheckDetailDiagnosticKind::Unavailable,
                &format!(
                    "Resolver exited unsuccessfully: {}",
                    redact(&String::from_utf8_lossy(&output.stderr), &secrets, 512)
                ),
            );
        }
        let mut detail = match serde_json::from_slice::<ExternalCheckDetailResponse>(&output.stdout)
        {
            Ok(detail) => detail,
            Err(_) => {
                return hydrate_diagnostic(
                    &request.provider,
                    ExtensionApiExternalCheckDetailDiagnosticKind::Malformed,
                    "Resolver returned malformed JSON.",
                );
            }
        };
        if detail.schema != EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA
            || detail.provider != request.provider
        {
            return hydrate_diagnostic(
                &request.provider,
                ExtensionApiExternalCheckDetailDiagnosticKind::MalformedIdentity,
                "Resolver response schema or provider did not match its declaration.",
            );
        }
        detail.build_id = detail.build_id.map(|value| redact(&value, &secrets, 512));
        detail.summary = detail.summary.map(|value| redact(&value, &secrets, 2048));
        detail.actions = detail
            .actions
            .into_iter()
            .map(|value| redact(&value, &secrets, 512))
            .collect();
        detail.artifact_refs = detail
            .artifact_refs
            .into_iter()
            .map(|value| redact(&value, &secrets, 2048))
            .collect();
        detail.log_refs = detail
            .log_refs
            .into_iter()
            .map(|value| redact(&value, &secrets, 2048))
            .collect();

        ExtensionApiExternalCheckDetailHydrateResponse {
            schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            detail: Some(detail),
            diagnostic: None,
            failure: None,
        }
    }
}

fn resolver_candidates(
    api_version: homeboy_extension_contract::api::v1::ExtensionApiVersion,
) -> Result<Vec<ResolverCandidate>, ExtensionApiOperationFailure> {
    let snapshot = snapshot_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version,
    });
    if let Some(failure) = snapshot.response.failure {
        return Err(failure);
    }

    let mut candidates = Vec::new();
    for catalog_entry in snapshot.response.entries {
        let Some(manifest) = snapshot.manifests.get(&catalog_entry.id) else {
            continue;
        };
        let advertised_capabilities = catalog_entry
            .descriptor
            .as_ref()
            .into_iter()
            .flat_map(|descriptor| &descriptor.capabilities)
            .map(|capability| capability.id.as_str())
            .filter(|id| id.starts_with(EXTERNAL_CHECK_DETAIL_RESOLVER_CAPABILITY_PREFIX))
            .collect::<Vec<_>>();
        for declaration in &manifest.external_check_detail_resolvers {
            let provider = declaration.declared_provider();
            let config = match declaration {
                ExternalCheckDetailResolverDeclaration::Config(config) => {
                    config.validate().ok().map(|_| config.as_ref().clone())
                }
                ExternalCheckDetailResolverDeclaration::Malformed(_) => None,
            };
            let capability_advertised = provider.as_ref().is_some_and(|provider| {
                advertised_capabilities.iter().any(|capability| {
                    *capability
                        == format!("{EXTERNAL_CHECK_DETAIL_RESOLVER_CAPABILITY_PREFIX}{provider}")
                })
            });
            let valid = catalog_entry.status == ExtensionApiCatalogEntryStatus::Available
                && capability_advertised
                && config.is_some();
            candidates.push(ResolverCandidate {
                entry: ExtensionApiExternalCheckDetailInventoryEntry {
                    provider,
                    owning_extension: manifest.id.clone(),
                    resolvable: valid,
                    validation: if valid {
                        ExtensionApiExternalCheckDetailProviderValidation::Valid
                    } else {
                        ExtensionApiExternalCheckDetailProviderValidation::Invalid
                    },
                    diagnostic: (!valid).then(|| {
                        "Resolver requires a provider and valid literal command declaration."
                            .to_string()
                    }),
                },
                config: valid.then_some(config).flatten(),
                extension_path: manifest.extension_path.clone(),
            });
        }
    }
    mark_duplicates(&mut candidates);
    candidates.sort_by(|left, right| {
        (&left.entry.provider, &left.entry.owning_extension)
            .cmp(&(&right.entry.provider, &right.entry.owning_extension))
    });
    Ok(candidates)
}

fn mark_duplicates(candidates: &mut [ResolverCandidate]) {
    let counts = candidates
        .iter()
        .filter_map(|candidate| candidate.entry.provider.as_deref())
        .fold(BTreeMap::new(), |mut counts, provider| {
            *counts.entry(provider.to_string()).or_insert(0usize) += 1;
            counts
        });
    for candidate in candidates {
        if candidate.entry.validation == ExtensionApiExternalCheckDetailProviderValidation::Valid
            && candidate
                .entry
                .provider
                .as_ref()
                .is_some_and(|provider| counts.get(provider).copied().unwrap_or_default() > 1)
        {
            candidate.entry.resolvable = false;
            candidate.entry.validation =
                ExtensionApiExternalCheckDetailProviderValidation::Duplicate;
            candidate.entry.diagnostic =
                Some("Multiple installed extensions declare this provider.".to_string());
            candidate.config = None;
        }
    }
}

fn resolve_program(
    extension_path: &Path,
    resolver: &ExternalCheckDetailResolverConfig,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let root = extension_path
        .canonicalize()
        .map_err(|error| format!("extension root cannot be resolved: {error}"))?;
    let program = root
        .join(&resolver.command[0])
        .canonicalize()
        .map_err(|error| format!("program cannot be resolved inside the extension: {error}"))?;
    if !program.starts_with(&root) {
        return Err("program resolves outside the declaring extension".to_string());
    }
    Ok((root, program))
}

pub fn normalize_target_url(value: &str) -> String {
    let without_secret_suffix = value.split(['?', '#']).next().unwrap_or_default();
    let without_credentials = without_secret_suffix
        .split_once("//")
        .map(|(scheme, rest)| format!("{scheme}//{}", rest.rsplit('@').next().unwrap_or(rest)))
        .unwrap_or_else(|| without_secret_suffix.to_string());
    bound(&without_credentials, 2048)
}

fn redact(value: &str, secrets: &[String], limit: usize) -> String {
    let mut redacted = RedactionPolicy::default().redact_embedded_urls(value);
    for secret in secrets {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "[REDACTED]");
        }
    }
    bound(&redacted, limit)
}

fn bound(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn hydrate_failure(
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiExternalCheckDetailHydrateResponse {
    ExtensionApiExternalCheckDetailHydrateResponse {
        schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        detail: None,
        diagnostic: None,
        failure: Some(failure),
    }
}

fn hydrate_diagnostic(
    provider: &str,
    kind: ExtensionApiExternalCheckDetailDiagnosticKind,
    message: &str,
) -> ExtensionApiExternalCheckDetailHydrateResponse {
    ExtensionApiExternalCheckDetailHydrateResponse {
        schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        detail: None,
        diagnostic: Some(ExtensionApiExternalCheckDetailDiagnostic {
            provider: provider.to_string(),
            kind,
            message: message.to_string(),
        }),
        failure: None,
    }
}

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub fn test_inherited_pipe_holder_cleanup() {
    use std::os::unix::fs::PermissionsExt;

    let extension = tempfile::tempdir().unwrap();
    let pid_file = extension.path().join("descendant.pid");
    let script = extension.path().join("resolve");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsetsid sh -c 'echo $$ > \"$1\"; sleep 30' sh {} &\nwhile [ ! -s {} ]; do :; done\nexit 0\n",
            homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
            homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new(&script);
    command.current_dir(extension.path()).env_clear();

    let output = execute_deadline_process(
        command,
        b"{}",
        Instant::now() + Duration::from_millis(100),
        CLEANUP_BUDGET,
        CAPTURE_LIMIT_BYTES,
        "Resolver",
    )
    .expect("leader exit should return captured output after descendant cleanup");
    assert!(output.status.success());
    let descendant_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(
        !crate::process::pid_is_running(descendant_pid),
        "detached resolver descendant {descendant_pid} survived cleanup"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory_request() -> ExtensionApiExternalCheckDetailInventoryRequest {
        ExtensionApiExternalCheckDetailInventoryRequest {
            schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_INVENTORY_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        }
    }

    fn install_manifest(id: &str, resolvers: serde_json::Value) -> std::path::PathBuf {
        let extension = crate::paths::extensions().unwrap().join(id);
        std::fs::create_dir_all(&extension).unwrap();
        std::fs::write(
            extension.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "1.0.0",
                "external_check_detail_resolvers": resolvers
            })
            .to_string(),
        )
        .unwrap();
        extension
    }

    #[test]
    fn inventory_retains_safe_valid_malformed_and_duplicate_entries() {
        crate::test_support::with_isolated_home(|_| {
            install_manifest(
                "valid",
                serde_json::json!([{
                    "provider": "valid-ci",
                    "command": ["resolve"],
                    "secret_env": ["DO_NOT_EXPOSE_TOKEN"]
                }]),
            );
            install_manifest(
                "malformed",
                serde_json::json!([{
                    "provider": "malformed-ci",
                    "command": "private-command"
                }]),
            );
            for id in ["duplicate-a", "duplicate-b"] {
                install_manifest(
                    id,
                    serde_json::json!([{
                        "provider": "duplicate-ci",
                        "command": ["resolve"]
                    }]),
                );
            }

            let api = ExternalCheckDetailResolverApi::discover(&inventory_request());
            let inventory = api.inventory_api();
            assert_eq!(inventory.providers.len(), 4);
            assert!(inventory.providers.iter().any(|entry| {
                entry.provider.as_deref() == Some("valid-ci")
                    && entry.validation == ExtensionApiExternalCheckDetailProviderValidation::Valid
            }));
            assert!(inventory.providers.iter().any(|entry| {
                entry.provider.as_deref() == Some("malformed-ci")
                    && entry.validation
                        == ExtensionApiExternalCheckDetailProviderValidation::Invalid
            }));
            assert_eq!(
                inventory
                    .providers
                    .iter()
                    .filter(|entry| {
                        entry.provider.as_deref() == Some("duplicate-ci")
                            && entry.validation
                                == ExtensionApiExternalCheckDetailProviderValidation::Duplicate
                    })
                    .count(),
                2
            );
            let wire = serde_json::to_string(&inventory).unwrap();
            assert!(!wire.contains("private-command"));
            assert!(!wire.contains("DO_NOT_EXPOSE_TOKEN"));
            assert!(!wire.contains(
                crate::paths::extensions()
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            ));

            let catalog = crate::extension::catalog::list_api(&ExtensionApiCatalogRequest {
                schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
            });
            let capability = catalog
                .entries
                .iter()
                .find(|entry| entry.id == "valid")
                .and_then(|entry| entry.descriptor.as_ref())
                .and_then(|descriptor| {
                    descriptor.capabilities.iter().find(|capability| {
                        capability.id == "external-check-detail-resolver.valid-ci"
                    })
                })
                .expect("resolver capability");
            assert_eq!(
                capability
                    .input_schema
                    .as_ref()
                    .map(|schema| schema.schema.as_str()),
                Some(EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA)
            );
            assert_eq!(
                capability
                    .output_schema
                    .as_ref()
                    .map(|schema| schema.schema.as_str()),
                Some(EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA)
            );
        });
    }

    #[test]
    fn resolver_session_keeps_one_immutable_catalog_snapshot() {
        crate::test_support::with_isolated_home(|_| {
            let session = ExternalCheckDetailResolverApi::discover(&inventory_request());
            install_manifest(
                "late",
                serde_json::json!([{
                    "provider": "late-ci",
                    "command": ["resolve"]
                }]),
            );
            assert!(session.inventory_api().providers.is_empty());
            let no_environment = |_name: &str| None;
            let response = session.hydrate_api(
                &ExtensionApiExternalCheckDetailHydrateRequest {
                    schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA.to_string(),
                    api_version: EXTENSION_API_V1,
                    provider: "late-ci".to_string(),
                    status: "failure".to_string(),
                    target_url: None,
                },
                ExternalCheckDetailHydrationContext {
                    deadline: Instant::now() + Duration::from_secs(1),
                    resolve_environment: &no_environment,
                },
            );
            assert_eq!(
                response.diagnostic.unwrap().kind,
                ExtensionApiExternalCheckDetailDiagnosticKind::Unknown
            );
            assert_eq!(
                ExternalCheckDetailResolverApi::discover(&inventory_request())
                    .inventory_api()
                    .providers
                    .len(),
                1
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn hydration_redacts_secrets_and_expired_deadlines_prevent_spawn() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let extension = install_manifest(
                "fixture",
                serde_json::json!([{
                    "provider": "fixture-ci",
                    "command": ["resolve"],
                    "secret_env": ["FIXTURE_SECRET"]
                }]),
            );
            let marker = extension.join("spawned");
            let script = extension.join("resolve");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\ntouch {}\nprintf '{{\"schema\":\"homeboy/external-check-detail-response/v1\",\"provider\":\"fixture-ci\",\"summary\":\"%s\",\"actions\":[\"%s\"]}}\\n' \"$FIXTURE_SECRET\" \"$FIXTURE_SECRET\"\n",
                    homeboy_engine_primitives::shell::quote_path(&marker.to_string_lossy())
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let api = ExternalCheckDetailResolverApi::discover(&inventory_request());
            let request = ExtensionApiExternalCheckDetailHydrateRequest {
                schema: EXTENSION_API_EXTERNAL_CHECK_DETAIL_HYDRATE_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
                provider: "fixture-ci".to_string(),
                status: "failure secret-value".to_string(),
                target_url: Some("https://user:token@example.test/build?secret=yes".to_string()),
            };
            let resolve_environment =
                |name: &str| (name == "FIXTURE_SECRET").then(|| "secret-value".to_string());

            let expired = api.hydrate_api(
                &request,
                ExternalCheckDetailHydrationContext {
                    deadline: Instant::now(),
                    resolve_environment: &resolve_environment,
                },
            );
            assert_eq!(
                expired.diagnostic.unwrap().kind,
                ExtensionApiExternalCheckDetailDiagnosticKind::Unavailable
            );
            assert!(!marker.exists());

            let hydrated = api.hydrate_api(
                &request,
                ExternalCheckDetailHydrationContext {
                    deadline: Instant::now() + Duration::from_secs(2),
                    resolve_environment: &resolve_environment,
                },
            );
            let detail = hydrated.detail.expect("resolver detail");
            assert_eq!(detail.summary.as_deref(), Some("[REDACTED]"));
            assert_eq!(detail.actions, ["[REDACTED]"]);
            assert!(marker.exists());
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_pipe_holder_is_reaped() {
        test_inherited_pipe_holder_cleanup();
    }
}
