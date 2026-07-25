use crate::manifest::ExtensionManifest;
use homeboy_core::error::{Error, Result};
use homeboy_core::server::execute_local_command_in_dir;
use homeboy_engine_primitives::shell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct EnvProviderContribution {
    pub extension_id: String,
    pub version: String,
    pub script: String,
    pub public_env: Vec<(String, String)>,
    pub secret_env_names: Vec<String>,
}

pub fn declared_secret_names(extension_id: &str) -> Result<Vec<String>> {
    let extension = homeboy_core::extension_store::load_extension(extension_id)?;
    let Some(config) = extension.env_provider else {
        return Err(Error::validation_invalid_argument(
            "extension_env",
            format!("Extension '{extension_id}' does not declare an env_provider"),
            Some(extension_id.to_string()),
            None,
        ));
    };
    let mut names = config.secret_env;
    names.sort();
    names.dedup();
    Ok(names)
}

/// Resolve an installed extension's environment contribution on the machine
/// that will execute the workload. The caller retains only this non-secret
/// provenance; secret values remain in the runner's secret-env resolver.
pub fn resolve_installed(
    extension_id: &str,
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<EnvProviderContribution> {
    let extension = homeboy_core::extension_store::load_extension(extension_id)?;
    let Some(script) = extension.env_provider_script() else {
        return Err(Error::validation_invalid_argument(
            "extension_env",
            format!("Extension '{extension_id}' does not declare an env_provider"),
            Some(extension_id.to_string()),
            Some(vec![format!(
                "Add env_provider.script to the '{extension_id}' extension manifest."
            )]),
        ));
    };
    let secret_env_names = declared_secret_names(extension_id)?;
    let public_env = env_vars(&extension, component_path, base_env)?;
    for (name, _) in &public_env {
        if secret_env_names.iter().any(|secret| secret == name)
            || homeboy_core::redaction::RedactionPolicy::default().is_sensitive_key(name)
        {
            return Err(Error::validation_invalid_argument(
                "extension_env",
                format!("Extension '{extension_id}' emitted sensitive '{name}' as public env"),
                Some(name.clone()),
                Some(vec!["Declare secret names in env_provider.secret_env and resolve them through runner secret_env instead of provider output.".to_string()]),
            ));
        }
    }
    Ok(EnvProviderContribution {
        extension_id: extension.id.clone(),
        version: extension.version.clone(),
        script: script.to_string(),
        public_env,
        secret_env_names,
    })
}

/// Resolve providers in request order and reject any ambiguous contribution
/// before the workload can start. The returned values contain no secrets.
pub fn resolve_installed_all(
    extension_ids: &[String],
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<Vec<EnvProviderContribution>> {
    let mut effective_env = base_env.to_vec();
    let mut owners = HashMap::new();
    for (name, _) in base_env {
        owners.insert(name.clone(), "request".to_string());
    }
    let mut contributions = Vec::new();
    for extension_id in extension_ids {
        let contribution = resolve_installed(extension_id, component_path, &effective_env)?;
        for (name, value) in &contribution.public_env {
            if let Some(owner) = owners.get(name) {
                return Err(Error::validation_invalid_argument(
                    "extension_env",
                    format!(
                        "Extension '{}' contributes '{name}', which is already set by {owner}",
                        contribution.extension_id
                    ),
                    Some(name.clone()),
                    Some(vec!["Use one provider for each environment variable or remove the conflicting request environment value.".to_string()]),
                ));
            }
            owners.insert(name.clone(), contribution.extension_id.clone());
            effective_env.push((name.clone(), value.clone()));
        }
        contributions.push(contribution);
    }
    Ok(contributions)
}

pub(crate) fn env_vars(
    extension: &ExtensionManifest,
    component_path: &Path,
    base_env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let Some(script_path) = extension.env_provider_script() else {
        return Ok(Vec::new());
    };
    let extension_path = extension_path(extension)?;
    let command = shell::quote_path(&extension_path.join(script_path).to_string_lossy());
    let env_refs = base_env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let env = (!env_refs.is_empty()).then_some(env_refs.as_slice());
    let output =
        execute_local_command_in_dir(&command, Some(&component_path.to_string_lossy()), env);

    if !output.success {
        return Err(Error::internal_io(
            format!(
                "Extension '{}' env provider failed with exit code {}: {}",
                extension.id,
                output.exit_code,
                output.stderr.trim()
            ),
            Some("extension env provider".to_string()),
        ));
    }

    parse_env_provider_output(&output.stdout)
}

pub(crate) fn load_manifest_from_dir(extension_path: &Path) -> Result<ExtensionManifest> {
    let manifest_value = super::execution::load_extension_manifest_from_dir(extension_path)?;
    let mut manifest =
        serde_json::from_value::<ExtensionManifest>(manifest_value).map_err(|e| {
            Error::validation_invalid_json(e, Some("parse extension manifest".to_string()), None)
        })?;
    manifest.extension_path = Some(extension_path.to_string_lossy().to_string());
    Ok(manifest)
}

fn extension_path(extension: &ExtensionManifest) -> Result<PathBuf> {
    extension
        .extension_path
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Extension '{}' has no extension_path",
                extension.id
            ))
        })
}

fn parse_env_provider_output(stdout: &str) -> Result<Vec<(String, String)>> {
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }

    let values = serde_json::from_str::<HashMap<String, String>>(stdout.trim()).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse extension env provider output".to_string()),
            None,
        )
    })?;

    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blank_output_as_no_env() {
        assert!(parse_env_provider_output("\n").unwrap().is_empty());
    }

    #[test]
    fn parses_json_object_as_sorted_env_pairs() {
        let env = parse_env_provider_output(r#"{"B":"two","A":"one"}"#).unwrap();

        assert_eq!(
            env,
            vec![
                ("A".to_string(), "one".to_string()),
                ("B".to_string(), "two".to_string())
            ]
        );
    }
}
