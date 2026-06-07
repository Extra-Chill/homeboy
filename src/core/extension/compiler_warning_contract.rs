use std::path::Path;

use crate::core::engine::command::{wait_with_bounded_output, DEFAULT_CAPTURE_LIMIT_BYTES};

use super::ExtensionManifest;

#[derive(Debug, Clone, Copy)]
pub(crate) enum CompilerWarningContract {
    Warnings,
    Fixes,
}

impl CompilerWarningContract {
    fn script_path(self, extension: &ExtensionManifest) -> Option<&str> {
        match self {
            Self::Warnings => extension.compiler_warnings_script(),
            Self::Fixes => extension.compiler_warning_fixes_script(),
        }
    }
}

pub(crate) fn extensions_for_compiler_warning_contract(
    root: &Path,
    contract: CompilerWarningContract,
) -> Vec<ExtensionManifest> {
    let mut extensions = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(component) = crate::core::component::discover_from_portable(root) {
        if let Some(component_extensions) = component.extensions.as_ref() {
            for extension_id in component_extensions.keys() {
                let Ok(extension) = crate::core::extension::load_extension(extension_id) else {
                    continue;
                };
                if contract.script_path(&extension).is_some() && seen.insert(extension.id.clone()) {
                    extensions.push(extension);
                }
            }
        }
    }

    if extensions.is_empty() {
        extensions.extend(
            crate::core::extension::load_all_extensions()
                .unwrap_or_default()
                .into_iter()
                .filter(|extension| contract.script_path(extension).is_some()),
        );
    }

    extensions
}

pub(crate) fn run_compiler_warning_contract_script(
    extension: &ExtensionManifest,
    contract: CompilerWarningContract,
    root: &Path,
    input: &serde_json::Value,
) -> Result<Option<String>, String> {
    let Some(extension_path) = extension.extension_path.as_deref() else {
        return Ok(None);
    };
    let Some(script_rel) = contract.script_path(extension) else {
        return Ok(None);
    };
    let script_path = Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return Ok(None);
    }

    let Some(output) = std::process::Command::new(&script_path)
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES).ok()
        })
    else {
        return Err(format!(
            "Failed to run compiler warning script for extension '{}'",
            extension.id
        ));
    };

    if !output.status.success() {
        return Err(format!(
            "Compiler warning script failed for extension '{}': {}",
            extension.id,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn test_extensions_for_compiler_warning_contract_prefers_component_extensions() {
        crate::test_support::with_isolated_home(|home| {
            for (id, script) in [("selected", "warnings.sh"), ("fallback", "warnings.sh")] {
                let extension_dir = home.path().join(format!(".config/homeboy/extensions/{id}"));
                fs::create_dir_all(extension_dir.join("scripts")).unwrap();
                fs::write(
                    extension_dir.join(format!("{id}.json")),
                    format!(
                        r#"{{"name":"{id}","version":"1.0.0","scripts":{{"compiler_warnings":"scripts/{script}"}}}}"#
                    ),
                )
                .unwrap();
            }

            let root = TempDir::new().expect("temp dir");
            fs::write(
                root.path().join("homeboy.json"),
                r#"{"id":"example","extensions":{"selected":{}}}"#,
            )
            .unwrap();

            let extensions = extensions_for_compiler_warning_contract(
                root.path(),
                CompilerWarningContract::Warnings,
            );

            assert_eq!(extensions.len(), 1);
            assert_eq!(extensions[0].id, "selected");
        });
    }

    #[test]
    fn test_run_compiler_warning_contract_script_returns_stdout() {
        let dir = TempDir::new().expect("temp dir");
        let script_path = dir.path().join("warnings.sh");
        write_executable(
            &script_path,
            "#!/usr/bin/env bash\ncat >/dev/null\nprintf 'ok'\n",
        );

        let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Example",
            "version": "1.0.0"
        }))
        .unwrap();
        extension.id = "example".to_string();
        extension.extension_path = Some(dir.path().to_string_lossy().to_string());
        extension.scripts = Some(crate::core::extension::ScriptsConfig {
            compiler_warnings: Some("warnings.sh".to_string()),
            ..Default::default()
        });

        let stdout = run_compiler_warning_contract_script(
            &extension,
            CompilerWarningContract::Warnings,
            dir.path(),
            &serde_json::json!({ "root": dir.path() }),
        )
        .unwrap();

        assert_eq!(stdout.as_deref(), Some("ok"));
    }
}
