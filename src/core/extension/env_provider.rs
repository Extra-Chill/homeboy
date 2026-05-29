use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::extension::manifest::ExtensionManifest;
use crate::core::server::execute_local_command_in_dir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
