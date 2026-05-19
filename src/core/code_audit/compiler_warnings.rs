//! Surface compiler warnings (dead code, unused imports, unused variables) as audit findings.
//!
//! Runs extension-owned compiler/checker scripts and maps their structured output
//! into audit findings.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/636

use std::path::Path;

use super::{AuditFinding, Finding, Severity};
use crate::core::extension::{
    extensions_for_compiler_warning_contract, run_compiler_warning_contract_script,
    CompilerWarningContract, ExtensionManifest,
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

/// Run compiler checks and return findings for any warnings detected.
pub fn run(root: &Path) -> Vec<Finding> {
    extensions_for_compiler_warning_contract(root, CompilerWarningContract::Warnings)
        .into_iter()
        .flat_map(|extension| run_extension_compiler_warnings(&extension, root))
        .collect()
}

fn run_extension_compiler_warnings(extension: &ExtensionManifest, root: &Path) -> Vec<Finding> {
    let Some(envelope) = run_compiler_warning_script(extension, root) else {
        return Vec::new();
    };

    envelope
        .warnings
        .into_iter()
        .filter(|warning| !warning.file.is_empty() && !warning.file.starts_with('/'))
        .map(|warning| Finding {
            file: warning.file.clone(),
            kind: AuditFinding::CompilerWarning,
            severity: Severity::Warning,
            convention: "compiler".to_string(),
            description: format!("[{}] {}", warning.code, warning.message),
            suggestion: warning
                .suggestion
                .unwrap_or_else(|| format!("Address compiler warning: {}", warning.code)),
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
    fn run_uses_extension_compiler_warning_script() {
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
            let findings = run(root.path());

            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].file, "src/lib.rs");
            assert_eq!(findings[0].kind, AuditFinding::CompilerWarning);
            assert_eq!(findings[0].description, "[unused_imports] unused import");
            assert_eq!(findings[0].suggestion, "Remove import");
        });
    }

    #[test]
    fn run_returns_no_findings_without_extension_contract() {
        crate::test_support::with_isolated_home(|_| {
            let dir = TempDir::new().expect("temp dir");
            fs::write(
                dir.path().join("Cargo.toml"),
                "[package]\nname = \"test-warn\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .unwrap();

            assert!(run(dir.path()).is_empty());
        });
    }
}
