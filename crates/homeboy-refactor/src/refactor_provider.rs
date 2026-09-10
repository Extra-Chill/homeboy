use std::path::Path;

use homeboy_core::extension::{catalog, invoke::invoke_api};
use homeboy_extension_contract::api::v1::{
    ExtensionApiCatalogEntryStatus, ExtensionApiCatalogRequest, ExtensionApiInvokeRequest,
    ExtensionApiInvokeResponse, EXTENSION_API_CATALOG_REQUEST_SCHEMA,
    EXTENSION_API_INVOKE_REQUEST_SCHEMA, EXTENSION_API_V1, REFACTOR_FILE_CAPABILITY_PREFIX,
};

pub(crate) type ParsedItem = homeboy_engine_primitives::grammar::items::GrammarItem;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResolvedImports {
    pub needed_imports: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RelatedTests {
    pub tests: Vec<ParsedItem>,
    #[serde(default)]
    pub ambiguous: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct AdjustedItem {
    pub source: String,
    pub changed: bool,
    pub original_visibility: String,
    pub new_visibility: String,
}

/// A refactor provider projected from Extension API v1 capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefactorProvider {
    pub(crate) extension_id: String,
    pub(crate) file_extensions: Vec<String>,
}

impl RefactorProvider {
    pub(crate) fn handles_file_extension(&self, file_extension: &str) -> bool {
        self.file_extensions
            .iter()
            .any(|extension| extension == file_extension)
    }

    fn capability_id(&self, file_extension: &str) -> Option<String> {
        self.handles_file_extension(file_extension)
            .then(|| format!("{REFACTOR_FILE_CAPABILITY_PREFIX}{file_extension}"))
    }
}

pub(crate) fn refactor_providers() -> Vec<RefactorProvider> {
    let catalog = catalog::list_api(&ExtensionApiCatalogRequest {
        schema: EXTENSION_API_CATALOG_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
    });
    let mut providers = catalog
        .entries
        .into_iter()
        .filter(|entry| entry.status == ExtensionApiCatalogEntryStatus::Available)
        .filter_map(|entry| {
            let descriptor = entry.descriptor?;
            let mut file_extensions = descriptor
                .capabilities
                .into_iter()
                .filter_map(|capability| {
                    capability
                        .id
                        .strip_prefix(REFACTOR_FILE_CAPABILITY_PREFIX)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            file_extensions.sort();
            file_extensions.dedup();
            (!file_extensions.is_empty()).then_some(RefactorProvider {
                extension_id: descriptor.identity.id,
                file_extensions,
            })
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| left.extension_id.cmp(&right.extension_id));
    providers
}

pub(crate) fn find_refactor_provider(
    root: &Path,
    file_extension: &str,
) -> Option<RefactorProvider> {
    let capability_id = format!("{REFACTOR_FILE_CAPABILITY_PREFIX}{file_extension}");
    let provider_ids = catalog::capability_provider_ids(root, &capability_id);
    refactor_providers().into_iter().find(|provider| {
        provider_ids.contains(&provider.extension_id)
            && provider.handles_file_extension(file_extension)
    })
}

pub(crate) fn invoke_refactor(
    provider: &RefactorProvider,
    working_directory: &Path,
    file_extension: &str,
    input: serde_json::Value,
) -> ExtensionApiInvokeResponse {
    let Some(capability_id) = provider.capability_id(file_extension) else {
        return ExtensionApiInvokeResponse {
            schema: homeboy_extension_contract::api::v1::EXTENSION_API_INVOKE_RESPONSE_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            output: None,
            failure: Some(homeboy_extension_contract::api::v1::ExtensionApiOperationFailure {
                code: homeboy_extension_contract::api::v1::ExtensionApiOperationFailureCode::CapabilityNotProvided,
                message: format!(
                    "Extension '{}' does not provide refactor.{}",
                    provider.extension_id, file_extension
                ),
            }),
            process: None,
        };
    };
    invoke_api(&ExtensionApiInvokeRequest {
        schema: EXTENSION_API_INVOKE_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: provider.extension_id.clone(),
        capability_id,
        working_directory: working_directory.to_string_lossy().into_owned(),
        input,
    })
}

pub(crate) fn invoke_refactor_value(
    provider: &RefactorProvider,
    working_directory: &Path,
    file_extension: &str,
    input: serde_json::Value,
) -> Option<serde_json::Value> {
    let response = invoke_refactor(provider, working_directory, file_extension, input);
    if let Some(output) = response.output {
        return Some(output);
    }
    if let Some(stderr) = response
        .process
        .and_then(|process| (!process.stderr.is_empty()).then_some(process.stderr))
    {
        homeboy_core::log_status!("refactor", "Extension script error: {}", stderr.trim());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn selects_the_requested_capability_for_multi_language_providers() {
        let provider = RefactorProvider {
            extension_id: "polyglot".to_string(),
            file_extensions: vec!["php".to_string(), "rs".to_string()],
        };

        assert_eq!(provider.capability_id("rs").as_deref(), Some("refactor.rs"));
        assert_eq!(
            provider.capability_id("php").as_deref(),
            Some("refactor.php")
        );
        assert_eq!(provider.capability_id("go"), None);
    }

    #[test]
    fn invokes_materially_different_refactor_capabilities_through_v1() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/polyglot");
            fs::create_dir_all(extension_dir.join("scripts")).unwrap();
            fs::write(
                extension_dir.join("polyglot.json"),
                r#"{"name":"Polyglot","version":"1.0.0","scripts":{"refactor":"scripts/refactor.sh"},"provides":{"file_extensions":["php","rs"]}}"#,
            )
            .unwrap();
            let script = extension_dir.join("scripts/refactor.sh");
            fs::write(&script, "#!/bin/sh\ncat\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(&script).unwrap().permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&script, permissions).unwrap();
            }
            let root = tempfile::TempDir::new().unwrap();

            for file_extension in ["php", "rs"] {
                let provider = find_refactor_provider(root.path(), file_extension)
                    .expect("provider for requested refactor capability");
                let input = serde_json::json!({
                    "command": "parse_items",
                    "file_extension": file_extension,
                });
                let response =
                    invoke_refactor(&provider, root.path(), file_extension, input.clone());

                assert_eq!(response.output, Some(input));
                assert!(response.failure.is_none());
            }
        });
    }
}
