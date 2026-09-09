//! Extension-side implementation of the audit compiler-warning provider.
//!
//! The audit engine (`code_audit`) defines `CompilerWarningProvider` and calls it
//! to collect compiler/checker warnings for a component, without depending on the
//! extension script runner. This module implements that trait by finding the
//! extensions that declare a compiler-warning script, running them, and parsing
//! their JSON envelopes into the slim `AuditCompilerWarning` view the audit
//! engine consumes. It is registered at binary startup by the CLI, mirroring the
//! fingerprint-script / grammar-source / component / fixability /
//! extension-manifest / runner-evidence provider hooks.

use std::path::Path;

use homeboy_core::code_audit::compiler_warning_provider::{
    register_compiler_warning_provider, AuditCompilerWarning, CompilerWarningProvider,
};

use super::{catalog::capability_provider_ids, invoke::invoke_api};
use homeboy_extension_contract::api::v1::{
    ExtensionApiInvokeRequest, COMPILER_WARNINGS_CAPABILITY_ID,
    EXTENSION_API_INVOKE_REQUEST_SCHEMA, EXTENSION_API_V1,
};

#[derive(Debug, Clone, serde::Deserialize)]
struct CompilerWarning {
    code: String,
    message: String,
    file: String,
    #[serde(rename = "line")]
    _line: usize,
    #[serde(default)]
    suggestion: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct CompilerWarningEnvelope {
    #[serde(default)]
    warnings: Vec<CompilerWarning>,
}

struct ExtensionCompilerWarningProvider;

impl CompilerWarningProvider for ExtensionCompilerWarningProvider {
    fn compiler_warnings(&self, root: &Path) -> Vec<AuditCompilerWarning> {
        capability_provider_ids(root, COMPILER_WARNINGS_CAPABILITY_ID)
            .into_iter()
            .flat_map(|extension_id| run_extension_compiler_warnings(&extension_id, root))
            .collect()
    }
}

fn run_extension_compiler_warnings(extension_id: &str, root: &Path) -> Vec<AuditCompilerWarning> {
    let Some(envelope) = run_compiler_warning_script(extension_id, root) else {
        return Vec::new();
    };

    envelope
        .warnings
        .into_iter()
        .map(|warning| AuditCompilerWarning {
            code: warning.code,
            message: warning.message,
            file: warning.file,
            suggestion: warning.suggestion,
        })
        .collect()
}

fn run_compiler_warning_script(extension_id: &str, root: &Path) -> Option<CompilerWarningEnvelope> {
    let response = invoke_api(&ExtensionApiInvokeRequest {
        schema: EXTENSION_API_INVOKE_REQUEST_SCHEMA.to_string(),
        api_version: EXTENSION_API_V1,
        extension_id: extension_id.to_string(),
        capability_id: COMPILER_WARNINGS_CAPABILITY_ID.to_string(),
        working_directory: root.to_string_lossy().into_owned(),
        input: serde_json::json!({
            "root": root,
        }),
    });
    if let Some(failure) = response.failure {
        homeboy_core::log_status!("audit", "{}", failure.message);
        return None;
    }
    response
        .output
        .and_then(|output| serde_json::from_value(output).ok())
}

/// Register the extension-backed compiler-warning provider. Called once at binary
/// startup by the CLI.
pub fn register() {
    register_compiler_warning_provider(Box::new(ExtensionCompilerWarningProvider));
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
    fn provider_uses_extension_compiler_warning_script() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/example");
            fs::create_dir_all(extension_dir.join("scripts")).unwrap();
            fs::write(
                extension_dir.join("example.json"),
                r#"{
                    "name": "Example",
                    "version": "1.0.0",
                    "scripts": { "compiler_warnings": "scripts/warnings.sh" }
                }"#,
            )
            .unwrap();
            write_executable(
                &extension_dir.join("scripts/warnings.sh"),
                r#"#!/usr/bin/env bash
cat >/dev/null
printf '{"warnings":[{"code":"unused_imports","message":"unused import","file":"src/lib.rs","line":3,"suggestion":"Remove import"}]}'
"#,
            );

            let root = TempDir::new().expect("temp dir");
            let warnings = ExtensionCompilerWarningProvider.compiler_warnings(root.path());

            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].code, "unused_imports");
            assert_eq!(warnings[0].message, "unused import");
            assert_eq!(warnings[0].file, "src/lib.rs");
            assert_eq!(warnings[0].suggestion.as_deref(), Some("Remove import"));
        });
    }

    #[test]
    fn provider_returns_no_warnings_without_extension_contract() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let dir = TempDir::new().expect("temp dir");
            fs::write(
                dir.path().join("Cargo.toml"),
                "[package]\nname = \"test-warn\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .unwrap();

            assert!(ExtensionCompilerWarningProvider
                .compiler_warnings(dir.path())
                .is_empty());
        });
    }

    #[test]
    fn provider_prefers_component_linked_extensions() {
        homeboy_core::test_support::with_isolated_home(|home| {
            for id in ["selected", "fallback"] {
                let extension_dir = home.path().join(format!(".config/homeboy/extensions/{id}"));
                fs::create_dir_all(extension_dir.join("scripts")).unwrap();
                fs::write(
                    extension_dir.join(format!("{id}.json")),
                    format!(
                        r#"{{
                            "name": "{id}",
                            "version": "1.0.0",
                            "scripts": {{ "compiler_warnings": "scripts/warnings.sh" }}
                        }}"#
                    ),
                )
                .unwrap();
                write_executable(
                    &extension_dir.join("scripts/warnings.sh"),
                    &format!(
                        "#!/usr/bin/env bash\ncat >/dev/null\nprintf '{{\"warnings\":[{{\"code\":\"{id}\",\"message\":\"warning\",\"file\":\"src/lib.rs\",\"line\":1}}]}}'\n"
                    ),
                );
            }
            let root = TempDir::new().expect("temp dir");
            fs::write(
                root.path().join("homeboy.json"),
                r#"{"id":"example","extensions":{"selected":{}}}"#,
            )
            .unwrap();

            let warnings = ExtensionCompilerWarningProvider.compiler_warnings(root.path());

            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].code, "selected");
        });
    }
}
