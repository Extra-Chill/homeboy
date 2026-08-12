//! Manifest-backed extension-owned remote recipe execution descriptors.

use homeboy_core::error::{Error, Result};

use crate::manifest::ExtensionManifest;

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

/// Read recipe-run descriptors from an installed extension manifest.
pub fn recipe_run_providers(manifest: &ExtensionManifest) -> Vec<RecipeRunProviderDescriptor> {
    manifest
        .extra
        .get("recipe_run_providers")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Discover one installed extension-owned provider by its stable ID.
pub fn resolve_recipe_run_provider(id: &str) -> Result<RecipeRunProviderDescriptor> {
    let matches = crate::load_all_extensions()?
        .into_iter()
        .flat_map(|manifest| recipe_run_providers(&manifest))
        .filter(|provider| provider.id == id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [provider] => validate_descriptor(provider.clone()),
        [] => Err(Error::validation_invalid_argument(
            "provider",
            format!("No installed extension declares recipe-run provider '{id}'"),
            Some(id.to_string()),
            Some(vec![
                "Install an extension manifest with recipe_run_providers.".to_string(),
            ]),
        )),
        _ => Err(Error::validation_invalid_argument(
            "provider",
            format!("Multiple installed extensions declare recipe-run provider '{id}'"),
            Some(id.to_string()),
            None,
        )),
    }
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
}
