//! Manifest-backed extension-owned remote recipe execution descriptors.

use homeboy_core::error::{Error, Result};

use crate::manifest::ExtensionManifest;
use crate::DiscoveredExtension;

const AVAILABLE_ID_LIMIT: usize = 20;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct RecipeRunProviderDescriptor {
    pub id: String,
    pub version: String,
    pub executable: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRunRequest {
    pub recipe_path: String,
    pub artifact_path: String,
}

/// One installed extension declaration as surfaced by recipe-run diagnostics.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct RecipeRunProviderInventoryEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub owning_extension: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// True only when this installed declaration is structurally resolvable.
    /// Executable readiness remains runner-specific and is unprobed here.
    pub resolvable: bool,
    pub validation: RecipeRunProviderValidation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecipeRunProviderValidation {
    Valid,
    Invalid,
    Duplicate,
}

#[derive(Debug, Clone)]
struct DiscoveredProvider {
    entry: RecipeRunProviderInventoryEntry,
    descriptor: Option<RecipeRunProviderDescriptor>,
}

/// Read recipe-run descriptors from an installed extension manifest.
pub fn recipe_run_providers(manifest: &ExtensionManifest) -> Vec<RecipeRunProviderDescriptor> {
    manifest
        .extra
        .get("recipe_run_providers")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// List every installed recipe-run declaration, including malformed entries.
pub fn recipe_run_provider_inventory() -> Vec<RecipeRunProviderInventoryEntry> {
    let mut providers = discover_recipe_run_providers();
    let duplicate_ids = providers
        .iter()
        .filter_map(|provider| provider.entry.id.as_deref())
        .fold(std::collections::BTreeMap::new(), |mut counts, id| {
            *counts.entry(id.to_string()).or_insert(0usize) += 1;
            counts
        });
    for provider in &mut providers {
        if provider.entry.validation == RecipeRunProviderValidation::Valid
            && provider
                .entry
                .id
                .as_ref()
                .is_some_and(|id| duplicate_ids.get(id).copied().unwrap_or_default() > 1)
        {
            provider.entry.resolvable = false;
            provider.entry.validation = RecipeRunProviderValidation::Duplicate;
            provider.entry.diagnostic =
                Some("Multiple installed extensions declare this provider ID.".to_string());
        }
    }
    providers
        .into_iter()
        .map(|provider| provider.entry)
        .collect()
}

/// Discover one installed extension-owned provider by its stable ID.
pub fn resolve_recipe_run_provider(id: &str) -> Result<RecipeRunProviderDescriptor> {
    let inventory = recipe_run_provider_inventory();
    let matches = inventory
        .iter()
        .filter(|provider| provider.id.as_deref() == Some(id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [provider] if provider.validation == RecipeRunProviderValidation::Valid => {
            discover_recipe_run_providers()
                .into_iter()
                .find(|candidate| candidate.entry.id.as_deref() == Some(id))
                .and_then(|candidate| candidate.descriptor)
                .ok_or_else(|| malformed_provider_error(id))
        }
        [] => {
            let mut error = Error::validation_invalid_argument(
                "provider",
                format!("No installed extension declares recipe-run provider '{id}'"),
                Some(id.to_string()),
                None,
            );
            for hint in unknown_provider_hints(&inventory) {
                error = error.with_hint(hint);
            }
            Err(error)
        }
        [provider] => Err(Error::validation_invalid_argument(
            "provider",
            format!(
                "Installed recipe-run provider '{id}' is {}",
                provider.validation_name()
            ),
            Some(id.to_string()),
            None,
        )),
        _ => Err(Error::validation_invalid_argument(
            "provider",
            format!("Multiple installed extensions declare recipe-run provider '{id}'"),
            Some(id.to_string()),
            None,
        )),
    }
}

impl RecipeRunProviderInventoryEntry {
    fn validation_name(&self) -> &'static str {
        match self.validation {
            RecipeRunProviderValidation::Valid => "valid",
            RecipeRunProviderValidation::Invalid => "invalid",
            RecipeRunProviderValidation::Duplicate => "declared more than once",
        }
    }
}

fn discover_recipe_run_providers() -> Vec<DiscoveredProvider> {
    let mut providers = crate::discover_extensions()
        .into_iter()
        .flat_map(|extension| match extension {
            DiscoveredExtension::Valid(manifest) => declarations_from_manifest(&manifest),
            DiscoveredExtension::Invalid(_) => Vec::new(),
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| {
        (&left.entry.id, &left.entry.owning_extension)
            .cmp(&(&right.entry.id, &right.entry.owning_extension))
    });
    providers
}

fn declarations_from_manifest(manifest: &ExtensionManifest) -> Vec<DiscoveredProvider> {
    let Some(value) = manifest.extra.get("recipe_run_providers") else {
        return Vec::new();
    };
    let source_ref = manifest.extension_path.clone();
    let declarations = value
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![value.clone()]);
    declarations
        .into_iter()
        .map(|value| {
            let id = value.get("id").and_then(serde_json::Value::as_str).map(str::to_string);
            let version = value.get("version").and_then(serde_json::Value::as_str).map(str::to_string);
            let executable = value.get("executable").and_then(serde_json::Value::as_str).map(str::to_string);
            let descriptor = serde_json::from_value::<RecipeRunProviderDescriptor>(value);
            match descriptor
                .ok()
                .and_then(|descriptor| validate_descriptor(descriptor).ok())
            {
                Some(descriptor) => DiscoveredProvider {
                    entry: RecipeRunProviderInventoryEntry { id, version, owning_extension: manifest.id.clone(), source_ref: source_ref.clone(), executable, resolvable: true, validation: RecipeRunProviderValidation::Valid, diagnostic: None },
                    descriptor: Some(descriptor),
                },
                None => DiscoveredProvider {
                    entry: RecipeRunProviderInventoryEntry { id, version, owning_extension: manifest.id.clone(), source_ref: source_ref.clone(), executable, resolvable: false, validation: RecipeRunProviderValidation::Invalid, diagnostic: Some("Provider requires id, version, executable, and argv beginning with executable.".to_string()) },
                    descriptor: None,
                },
            }
        })
        .collect()
}

fn malformed_provider_error(id: &str) -> Error {
    Error::validation_invalid_argument(
        "provider",
        format!("Installed recipe-run provider '{id}' is invalid"),
        Some(id.to_string()),
        None,
    )
}

fn unknown_provider_hints(inventory: &[RecipeRunProviderInventoryEntry]) -> Vec<String> {
    let available = inventory
        .iter()
        .filter(|provider| provider.resolvable)
        .filter_map(|provider| provider.id.as_deref())
        .take(AVAILABLE_ID_LIMIT)
        .collect::<Vec<_>>();
    let mut hints =
        vec!["Run 'homeboy runner recipe-providers' to inspect installed providers.".to_string()];
    if !available.is_empty() {
        hints.push(format!("Available provider IDs: {}", available.join(", ")));
    }
    hints
}

impl RecipeRunProviderDescriptor {
    /// Render the extension-declared argv without introducing shell parsing.
    pub fn render(&self, request: &RecipeRunRequest) -> Result<Vec<String>> {
        validate_descriptor(self.clone())?;
        Ok(self
            .command
            .iter()
            .map(|argument| {
                argument
                    .replace("{recipe}", &request.recipe_path)
                    .replace("{artifacts}", &request.artifact_path)
            })
            .collect())
    }
}

fn validate_descriptor(
    descriptor: RecipeRunProviderDescriptor,
) -> Result<RecipeRunProviderDescriptor> {
    if descriptor.id.trim().is_empty()
        || descriptor.version.trim().is_empty()
        || descriptor.executable.trim().is_empty()
        || descriptor.command.is_empty()
        || descriptor.command.first() != Some(&descriptor.executable)
    {
        return Err(Error::validation_invalid_argument(
            "provider",
            "recipe-run provider requires id, version, executable, and argv beginning with executable",
            Some(descriptor.id),
            None,
        ));
    }
    Ok(descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_manifest_owned_argv_without_shell_parsing() {
        let provider = RecipeRunProviderDescriptor {
            id: "fixture.recipe-run".to_string(),
            version: "1.0.0".to_string(),
            executable: "fixture-run".to_string(),
            command: vec![
                "fixture-run".to_string(),
                "--recipe".to_string(),
                "{recipe}".to_string(),
                "--artifacts={artifacts}".to_string(),
            ],
        };
        assert_eq!(
            provider
                .render(&RecipeRunRequest {
                    recipe_path: "recipe.json".to_string(),
                    artifact_path: "artifacts".to_string(),
                })
                .expect("render"),
            vec![
                "fixture-run",
                "--recipe",
                "recipe.json",
                "--artifacts=artifacts"
            ]
        );
    }

    #[test]
    fn discovers_a_descriptor_from_an_installed_extension() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("extension source");
            std::fs::write(
                source.path().join("fixture.json"),
                serde_json::json!({
                    "name": "Recipe fixture",
                    "version": "1.0.0",
                    "recipe_run_providers": [{
                        "id": "fixture.recipe-run",
                        "version": "1.0.0",
                        "executable": "fixture-run",
                        "command": ["fixture-run", "--recipe", "{recipe}", "--artifacts", "{artifacts}"],
                    }],
                })
                .to_string(),
            )
            .expect("manifest");
            crate::install(&source.path().display().to_string(), Some("fixture"))
                .expect("install extension");

            let provider = resolve_recipe_run_provider("fixture.recipe-run").expect("discover");
            assert_eq!(provider.version, "1.0.0");
            assert_eq!(
                provider
                    .render(&RecipeRunRequest {
                        recipe_path: "recipe.json".to_string(),
                        artifact_path: "artifacts".to_string(),
                    })
                    .expect("render"),
                vec![
                    "fixture-run",
                    "--recipe",
                    "recipe.json",
                    "--artifacts",
                    "artifacts"
                ]
            );
        });
    }

    #[test]
    fn inventories_valid_invalid_and_duplicate_declarations() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("extension source");
            std::fs::write(
                source.path().join("fixture.json"),
                serde_json::json!({
                    "name": "Recipe fixture",
                    "version": "1.0.0",
                    "recipe_run_providers": [
                        { "id": "fixture.valid", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] },
                        { "id": "fixture.invalid", "version": "1.0.0", "executable": "fixture-run", "command": ["other-run"] },
                        { "id": "fixture.duplicate", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] }
                    ]
                })
                .to_string(),
            )
            .expect("manifest");
            crate::install(&source.path().display().to_string(), Some("fixture")).expect("install");
            let duplicate_source = tempfile::tempdir().expect("duplicate extension source");
            std::fs::write(
                duplicate_source.path().join("duplicate.json"),
                serde_json::json!({
                    "name": "Duplicate fixture", "version": "1.0.0",
                    "recipe_run_providers": [{ "id": "fixture.duplicate", "version": "1.0.0", "executable": "fixture-run", "command": ["fixture-run"] }]
                })
                .to_string(),
            )
            .expect("duplicate manifest");
            crate::install(
                &duplicate_source.path().display().to_string(),
                Some("duplicate"),
            )
            .expect("install duplicate");

            let inventory = recipe_run_provider_inventory();
            assert_eq!(inventory.len(), 4);
            assert!(inventory
                .iter()
                .any(|provider| provider.id.as_deref() == Some("fixture.valid")
                    && provider.resolvable));
            assert!(inventory
                .iter()
                .any(|provider| provider.id.as_deref() == Some("fixture.invalid")
                    && provider.validation == RecipeRunProviderValidation::Invalid));
            assert_eq!(
                inventory
                    .iter()
                    .filter(
                        |provider| provider.id.as_deref() == Some("fixture.duplicate")
                            && provider.validation == RecipeRunProviderValidation::Duplicate
                    )
                    .count(),
                2
            );
            assert!(resolve_recipe_run_provider("fixture.duplicate").is_err());
        });
    }

    #[test]
    fn unknown_provider_diagnostic_has_inventory_command_and_bounded_ids() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let error =
                resolve_recipe_run_provider("fixture.missing").expect_err("missing provider");
            let hints = error.hints;
            assert_eq!(
                hints[0].message,
                "Run 'homeboy runner recipe-providers' to inspect installed providers."
            );
        });
    }
}
