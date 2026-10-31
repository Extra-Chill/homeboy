//! Typed Extension API inventory and planning for recipe-run providers.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest, ExtensionApiOperationFailure,
    ExtensionApiOperationFailureCode, ExtensionApiRecipeRunPlan, ExtensionApiRecipeRunPlanRequest,
    ExtensionApiRecipeRunPlanResponse, ExtensionApiRecipeRunProviderInventoryEntry,
    ExtensionApiRecipeRunProviderInventoryRequest, ExtensionApiRecipeRunProviderInventoryResponse,
    ExtensionApiRecipeRunProviderValidation, ExtensionApiRecipeRunSelectionFailure,
    ExtensionApiRecipeRunSelectionFailureCode, EXTENSION_API_CATALOG_REQUEST_SCHEMA,
    EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA, EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA,
    EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_REQUEST_SCHEMA,
    EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_RESPONSE_SCHEMA, EXTENSION_API_V1,
    RECIPE_RUN_PROVIDER_CAPABILITY_PREFIX,
};
use homeboy_extension_contract::{
    ExtensionManifest, RecipeRunProviderDeclaration, RecipeRunProviderDescriptor,
};

use crate::extension::catalog::{snapshot_api, validate_operation_request};

const AVAILABLE_ID_LIMIT: usize = 20;

#[derive(Debug, Clone)]
struct RecipeRunProviderCandidate {
    entry: ExtensionApiRecipeRunProviderInventoryEntry,
    descriptor: Option<RecipeRunProviderDescriptor>,
}

pub fn recipe_run_provider_inventory_api(
    request: &ExtensionApiRecipeRunProviderInventoryRequest,
) -> ExtensionApiRecipeRunProviderInventoryResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return inventory_failure(failure);
    }

    match recipe_run_provider_candidates(request.api_version) {
        Ok(candidates) => ExtensionApiRecipeRunProviderInventoryResponse {
            schema: EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            providers: candidates
                .into_iter()
                .map(|candidate| candidate.entry)
                .collect(),
            failure: None,
        },
        Err(failure) => inventory_failure(failure),
    }
}

pub fn recipe_run_plan_api(
    request: &ExtensionApiRecipeRunPlanRequest,
) -> ExtensionApiRecipeRunPlanResponse {
    if let Some(failure) = validate_operation_request(
        &request.schema,
        EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA,
        request.api_version,
    ) {
        return plan_failure(failure, Vec::new());
    }
    for (name, value) in [
        ("recipe", request.recipe_path.as_str()),
        ("artifacts", request.artifact_path.as_str()),
    ] {
        if !is_workspace_relative_path(value) {
            return plan_failure(
                operation_failure(
                    ExtensionApiOperationFailureCode::InvalidRequestSchema,
                    format!(
                        "runner recipe-run --{name} must be a non-empty workspace-relative path without '..'"
                    ),
                ),
                Vec::new(),
            );
        }
    }

    let candidates = match recipe_run_provider_candidates(request.api_version) {
        Ok(candidates) => candidates,
        Err(failure) => return plan_failure(failure, Vec::new()),
    };
    let available_provider_ids = available_provider_ids(&candidates);
    let matches = candidates
        .iter()
        .filter(|candidate| candidate.entry.id.as_deref() == Some(&request.provider_id))
        .collect::<Vec<_>>();
    let candidate = match matches.as_slice() {
        [candidate]
            if candidate.entry.validation == ExtensionApiRecipeRunProviderValidation::Valid =>
        {
            *candidate
        }
        [] => {
            return plan_selection_failure(
                ExtensionApiRecipeRunSelectionFailureCode::NotFound,
                format!(
                    "No installed extension declares recipe-run provider '{}'",
                    request.provider_id
                ),
                available_provider_ids,
            );
        }
        [candidate] => {
            return plan_selection_failure(
                ExtensionApiRecipeRunSelectionFailureCode::Invalid,
                format!(
                    "Installed recipe-run provider '{}' is {}",
                    request.provider_id,
                    validation_name(candidate.entry.validation)
                ),
                available_provider_ids,
            );
        }
        _ => {
            return plan_selection_failure(
                ExtensionApiRecipeRunSelectionFailureCode::Ambiguous,
                format!(
                    "Multiple installed extensions declare recipe-run provider '{}'",
                    request.provider_id
                ),
                available_provider_ids,
            );
        }
    };
    let Some(descriptor) = candidate.descriptor.as_ref() else {
        return plan_selection_failure(
            ExtensionApiRecipeRunSelectionFailureCode::Invalid,
            format!(
                "Installed recipe-run provider '{}' is invalid",
                request.provider_id
            ),
            available_provider_ids,
        );
    };
    let command = descriptor
        .command
        .iter()
        .map(|argument| {
            argument
                .replace("{recipe}", &request.recipe_path)
                .replace("{artifacts}", &request.artifact_path)
        })
        .collect();

    ExtensionApiRecipeRunPlanResponse {
        schema: EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        plan: Some(ExtensionApiRecipeRunPlan {
            provider_id: descriptor.id.clone(),
            provider_version: descriptor.version.clone(),
            owning_extension: candidate.entry.owning_extension.clone(),
            command,
        }),
        available_provider_ids: Vec::new(),
        selection_failure: None,
        failure: None,
    }
}

fn recipe_run_provider_candidates(
    api_version: homeboy_extension_contract::api::v1::ExtensionApiVersion,
) -> Result<Vec<RecipeRunProviderCandidate>, ExtensionApiOperationFailure> {
    let snapshot = snapshot_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version,
    });
    let catalog = snapshot.response;
    let manifests = snapshot.manifests;
    if let Some(failure) = catalog.failure {
        return Err(failure);
    }

    let mut candidates = Vec::new();
    for catalog_entry in catalog.entries {
        let Some(descriptor) = catalog_entry.descriptor.as_ref() else {
            continue;
        };
        let manifest = manifests.get(&catalog_entry.id).ok_or_else(|| {
            operation_failure(
                ExtensionApiOperationFailureCode::ExtensionInvalid,
                format!(
                    "Catalog snapshot omitted manifest for extension '{}'",
                    catalog_entry.id
                ),
            )
        })?;
        let advertised_capabilities = descriptor
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .filter(|id| id.starts_with(RECIPE_RUN_PROVIDER_CAPABILITY_PREFIX))
            .collect::<Vec<_>>();
        candidates.extend(candidates_from_manifest(
            manifest,
            catalog_entry.status == ExtensionApiCatalogEntryStatus::Available,
            &advertised_capabilities,
        ));
    }
    mark_duplicate_provider_ids(&mut candidates);
    candidates.sort_by(|left, right| {
        (&left.entry.id, &left.entry.owning_extension)
            .cmp(&(&right.entry.id, &right.entry.owning_extension))
    });
    Ok(candidates)
}

fn candidates_from_manifest(
    manifest: &ExtensionManifest,
    extension_available: bool,
    advertised_capabilities: &[&str],
) -> Vec<RecipeRunProviderCandidate> {
    manifest
        .recipe_run_providers
        .iter()
        .map(|declaration| {
            let id = declaration.declared_str("id");
            let version = declaration.declared_str("version");
            let descriptor = match declaration {
                RecipeRunProviderDeclaration::Descriptor(descriptor) => {
                    validate_descriptor(descriptor.as_ref().clone()).ok()
                }
                RecipeRunProviderDeclaration::Malformed(_) => None,
            };
            let capability_advertised = id.as_ref().is_some_and(|id| {
                advertised_capabilities.iter().any(|capability| {
                    *capability == format!("{RECIPE_RUN_PROVIDER_CAPABILITY_PREFIX}{id}")
                })
            });
            let valid = extension_available && capability_advertised && descriptor.is_some();
            let diagnostic = if !extension_available {
                Some("Owning extension is incompatible with this Homeboy version.".to_string())
            } else if !capability_advertised || descriptor.is_none() {
                Some(
                    "Provider requires id, version, executable, and argv beginning with executable."
                        .to_string(),
                )
            } else {
                None
            };
            RecipeRunProviderCandidate {
                entry: ExtensionApiRecipeRunProviderInventoryEntry {
                    id,
                    version,
                    owning_extension: manifest.id.clone(),
                    resolvable: valid,
                    validation: if valid {
                        ExtensionApiRecipeRunProviderValidation::Valid
                    } else {
                        ExtensionApiRecipeRunProviderValidation::Invalid
                    },
                    diagnostic,
                },
                descriptor: valid.then_some(descriptor).flatten(),
            }
        })
        .collect()
}

fn mark_duplicate_provider_ids(candidates: &mut [RecipeRunProviderCandidate]) {
    let counts = candidates
        .iter()
        .filter_map(|candidate| candidate.entry.id.as_deref())
        .fold(BTreeMap::new(), |mut counts, id| {
            *counts.entry(id.to_string()).or_insert(0usize) += 1;
            counts
        });
    for candidate in candidates {
        if candidate.entry.validation == ExtensionApiRecipeRunProviderValidation::Valid
            && candidate
                .entry
                .id
                .as_ref()
                .is_some_and(|id| counts.get(id).copied().unwrap_or_default() > 1)
        {
            candidate.entry.resolvable = false;
            candidate.entry.validation = ExtensionApiRecipeRunProviderValidation::Duplicate;
            candidate.entry.diagnostic =
                Some("Multiple installed extensions declare this provider ID.".to_string());
            candidate.descriptor = None;
        }
    }
}

fn validate_descriptor(
    descriptor: RecipeRunProviderDescriptor,
) -> Result<RecipeRunProviderDescriptor, ()> {
    if descriptor.id.trim().is_empty()
        || descriptor.version.trim().is_empty()
        || descriptor.executable.trim().is_empty()
        || descriptor.command.is_empty()
        || descriptor.command.first() != Some(&descriptor.executable)
    {
        return Err(());
    }
    Ok(descriptor)
}

fn is_workspace_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn available_provider_ids(candidates: &[RecipeRunProviderCandidate]) -> Vec<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.entry.resolvable)
        .filter_map(|candidate| candidate.entry.id.clone())
        .take(AVAILABLE_ID_LIMIT)
        .collect()
}

fn validation_name(validation: ExtensionApiRecipeRunProviderValidation) -> &'static str {
    match validation {
        ExtensionApiRecipeRunProviderValidation::Valid => "valid",
        ExtensionApiRecipeRunProviderValidation::Invalid => "invalid",
        ExtensionApiRecipeRunProviderValidation::Duplicate => "declared more than once",
    }
}

fn operation_failure(
    code: ExtensionApiOperationFailureCode,
    message: String,
) -> ExtensionApiOperationFailure {
    ExtensionApiOperationFailure { code, message }
}

fn inventory_failure(
    failure: ExtensionApiOperationFailure,
) -> ExtensionApiRecipeRunProviderInventoryResponse {
    ExtensionApiRecipeRunProviderInventoryResponse {
        schema: EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        providers: Vec::new(),
        failure: Some(failure),
    }
}

fn plan_failure(
    failure: ExtensionApiOperationFailure,
    available_provider_ids: Vec<String>,
) -> ExtensionApiRecipeRunPlanResponse {
    ExtensionApiRecipeRunPlanResponse {
        schema: EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        plan: None,
        available_provider_ids,
        selection_failure: None,
        failure: Some(failure),
    }
}

fn plan_selection_failure(
    code: ExtensionApiRecipeRunSelectionFailureCode,
    message: String,
    available_provider_ids: Vec<String>,
) -> ExtensionApiRecipeRunPlanResponse {
    ExtensionApiRecipeRunPlanResponse {
        schema: EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        plan: None,
        available_provider_ids,
        selection_failure: Some(ExtensionApiRecipeRunSelectionFailure { code, message }),
        failure: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_extension(id: &str, providers: serde_json::Value) -> tempfile::TempDir {
        let source = tempfile::tempdir().expect("extension source");
        std::fs::write(
            source.path().join(format!("{id}.json")),
            serde_json::json!({
                "name": format!("{id} fixture"),
                "version": "1.0.0",
                "recipe_run_providers": providers,
            })
            .to_string(),
        )
        .expect("manifest");
        crate::extension::lifecycle::install(&source.path().display().to_string(), Some(id))
            .expect("install extension");
        source
    }

    fn inventory() -> ExtensionApiRecipeRunProviderInventoryResponse {
        recipe_run_provider_inventory_api(&ExtensionApiRecipeRunProviderInventoryRequest {
            schema: EXTENSION_API_RECIPE_RUN_PROVIDER_INVENTORY_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
        })
    }

    fn plan(provider_id: &str) -> ExtensionApiRecipeRunPlanResponse {
        recipe_run_plan_api(&ExtensionApiRecipeRunPlanRequest {
            schema: EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            provider_id: provider_id.to_string(),
            recipe_path: "recipe.json".to_string(),
            artifact_path: "artifacts".to_string(),
        })
    }

    #[test]
    fn plans_literal_argv_from_a_catalog_capability() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let _source = install_extension(
                "fixture",
                serde_json::json!([{
                    "id": "fixture.recipe-run",
                    "version": "1.2.3",
                    "executable": "fixture-run",
                    "command": ["fixture-run", "--recipe", "{recipe}", "--artifacts={artifacts}"]
                }]),
            );

            let response = plan("fixture.recipe-run");
            assert!(response.failure.is_none());
            assert_eq!(
                response.plan.expect("plan"),
                ExtensionApiRecipeRunPlan {
                    provider_id: "fixture.recipe-run".to_string(),
                    provider_version: "1.2.3".to_string(),
                    owning_extension: "fixture".to_string(),
                    command: vec![
                        "fixture-run".to_string(),
                        "--recipe".to_string(),
                        "recipe.json".to_string(),
                        "--artifacts=artifacts".to_string(),
                    ],
                }
            );

            let catalog = snapshot_api(&ExtensionApiCatalogRequest {
                schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
                api_version: EXTENSION_API_V1,
            })
            .response;
            let capability = catalog.entries[0]
                .descriptor
                .as_ref()
                .expect("descriptor")
                .capabilities
                .iter()
                .find(|capability| capability.id == "recipe-run-provider.fixture.recipe-run")
                .expect("recipe provider capability");
            assert_eq!(
                capability.input_schema.as_ref().expect("input").schema,
                EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA
            );
            assert_eq!(
                capability.output_schema.as_ref().expect("output").schema,
                EXTENSION_API_RECIPE_RUN_PLAN_RESPONSE_SCHEMA
            );
        });
    }

    #[test]
    fn malformed_declaration_remains_visible_without_exposing_implementation_fields() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let _source = install_extension(
                "fixture",
                serde_json::json!([
                    { "id": "fixture.valid", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] },
                    { "id": "fixture.no-version", "executable": "fixture-run", "command": ["fixture-run"] }
                ]),
            );

            let response = inventory();
            assert!(response.failure.is_none());
            assert_eq!(response.providers.len(), 2);
            let malformed = response
                .providers
                .iter()
                .find(|provider| provider.id.as_deref() == Some("fixture.no-version"))
                .expect("malformed provider");
            assert_eq!(
                malformed.validation,
                ExtensionApiRecipeRunProviderValidation::Invalid
            );
            assert!(!malformed.resolvable);
            let serialized = serde_json::to_value(&response).expect("inventory JSON");
            for provider in serialized["providers"].as_array().expect("providers") {
                assert!(provider.get("source_ref").is_none());
                assert!(provider.get("executable").is_none());
            }
            assert!(plan("fixture.valid").plan.is_some());
            assert_eq!(
                plan("fixture.no-version")
                    .selection_failure
                    .expect("failure")
                    .code,
                ExtensionApiRecipeRunSelectionFailureCode::Invalid
            );
        });
    }

    #[test]
    fn duplicate_and_missing_providers_return_typed_selection_failures() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let _first = install_extension(
                "first",
                serde_json::json!([
                    { "id": "fixture.available", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] },
                    { "id": "fixture.duplicate", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] }
                ]),
            );
            let _second = install_extension(
                "second",
                serde_json::json!([
                    { "id": "fixture.duplicate", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] }
                ]),
            );

            let duplicate = plan("fixture.duplicate");
            assert_eq!(
                duplicate.selection_failure.expect("duplicate failure").code,
                ExtensionApiRecipeRunSelectionFailureCode::Ambiguous
            );
            let missing = plan("fixture.missing");
            assert_eq!(
                missing.selection_failure.expect("missing failure").code,
                ExtensionApiRecipeRunSelectionFailureCode::NotFound
            );
            assert_eq!(
                missing.available_provider_ids,
                ["fixture.available".to_string()]
            );
        });
    }

    #[test]
    fn plan_rejects_non_workspace_relative_paths_before_discovery() {
        let response = recipe_run_plan_api(&ExtensionApiRecipeRunPlanRequest {
            schema: EXTENSION_API_RECIPE_RUN_PLAN_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            provider_id: "fixture".to_string(),
            recipe_path: "/recipe.json".to_string(),
            artifact_path: "../artifacts".to_string(),
        });

        assert_eq!(
            response.failure.expect("failure").code,
            ExtensionApiOperationFailureCode::InvalidRequestSchema
        );
    }
}
