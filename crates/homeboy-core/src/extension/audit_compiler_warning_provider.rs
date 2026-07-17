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

use crate::code_audit::compiler_warning_provider::{
    register_compiler_warning_provider, AuditCompilerWarning, CompilerWarningProvider,
};

use super::compiler_warning_contract::{
    extensions_for_compiler_warning_contract, run_compiler_warning_contract_script,
    CompilerWarningContract,
};
use super::ExtensionManifest;

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
        extensions_for_compiler_warning_contract(root, CompilerWarningContract::Warnings)
            .into_iter()
            .flat_map(|extension| run_extension_compiler_warnings(&extension, root))
            .collect()
    }
}

fn run_extension_compiler_warnings(
    extension: &ExtensionManifest,
    root: &Path,
) -> Vec<AuditCompilerWarning> {
    let Some(envelope) = run_compiler_warning_script(extension, root) else {
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

fn run_compiler_warning_script(
    extension: &ExtensionManifest,
    root: &Path,
) -> Option<CompilerWarningEnvelope> {
    let input = serde_json::json!({
        "root": root,
    });
    let stdout = match run_compiler_warning_contract_script(
        extension,
        CompilerWarningContract::Warnings,
        root,
        &input,
    ) {
        Ok(Some(stdout)) => stdout,
        Ok(None) => return None,
        Err(error) => {
            crate::log_status!("audit", "{}", error);
            return None;
        }
    };
    serde_json::from_str(&stdout).ok()
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
        crate::test_support::with_isolated_home(|home| {
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
        crate::test_support::with_isolated_home(|_| {
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
}
