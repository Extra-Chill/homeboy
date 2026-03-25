//! Language-agnostic function contract representation.
//!
//! A `FunctionContract` describes what a function promises: its signature,
//! control flow branches, side effects, and dependencies. Extensions produce
//! contracts from language-specific analysis; core consumes them for test
//! generation, documentation, refactor safety verification, and more.
//!
//! This follows the same architecture as fingerprinting:
//! - Core defines the struct and the consumer interface
//! - Extensions provide `scripts/contract.sh` to extract contracts
//! - Core never knows what language it's looking at
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/820

mod helpers;
mod types;

pub use helpers::*;
pub use types::*;


use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::error::{Error, Result};
use crate::extension;

// ── Core data types ──

// ── Control flow ──

// ── Effects ──

// ── Dependencies ──

// ── File-level container ──

// ── Type definitions ──

// ── Extraction API ──

/// Extract function contracts from a source file.
///
/// Uses a two-tier strategy:
/// 1. **Grammar-driven** (preferred): if the extension's grammar.toml has a `[contract]`
///    section, uses the core grammar engine to extract contracts. No subprocess needed.
/// 2. **Extension script** (fallback): if the extension has `scripts.contract`, runs
///    the script and parses JSON output.
///
/// Returns `None` if neither path is available.
pub fn extract_contracts(path: &Path, root: &Path) -> Result<Option<FileContracts>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();

    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    // Tier 1: Grammar-driven extraction (preferred — no subprocess)
    if let Some(grammar) = load_grammar_for_ext(ext) {
        if grammar.contract.is_some() {
            let content = std::fs::read_to_string(path).map_err(|e| {
                Error::internal_io(
                    format!("Failed to read source file: {}", e),
                    Some("extract_contracts".to_string()),
                )
            })?;

            if let Some(contracts) = super::contract_extract::extract_contracts_from_grammar(
                &content,
                &relative_path,
                &grammar,
            ) {
                return Ok(Some(FileContracts {
                    file: relative_path,
                    contracts,
                }));
            }
        }
    }

    // Tier 2: Extension script fallback
    let manifest = match find_extension_with_contract(ext) {
        Some(m) => m,
        None => return Ok(None),
    };

    let ext_path = manifest
        .extension_path
        .as_deref()
        .ok_or_else(|| Error::internal_unexpected("Extension has no path"))?;

    let script_rel = manifest
        .contract_script()
        .ok_or_else(|| Error::internal_unexpected("Extension has no contract script"))?;

    let script_path = std::path::Path::new(ext_path).join(script_rel);
    if !script_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            format!("Failed to read source file: {}", e),
            Some("extract_contracts".to_string()),
        )
    })?;

    // Extension contract script protocol:
    // - Receives JSON on stdin: { "file": "<relative_path>", "content": "<source>" }
    // - Outputs JSON on stdout: { "file": "<relative_path>", "contracts": [...] }
    let input = serde_json::json!({
        "file": relative_path,
        "content": content,
    });

    let input_json = serde_json::to_vec(&input).map_err(|e| {
        Error::internal_json(
            format!("Failed to serialize contract input: {}", e),
            Some("extract_contracts".to_string()),
        )
    })?;

    let mut child = std::process::Command::new("sh")
        .args([
            "-c",
            &format!(
                "sh {}",
                crate::engine::shell::quote_path(&script_path.to_string_lossy())
            ),
        ])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to spawn contract script: {}", e),
                Some("extract_contracts".to_string()),
            )
        })?;

    // Write input and close stdin
    {
        use std::io::Write;
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(&input_json);
            let _ = stdin.flush();
        }
    }
    child.stdin.take(); // Close stdin to signal EOF

    let output = child.wait_with_output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run contract script: {}", e),
            Some("extract_contracts".to_string()),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_io(
            format!("Contract script failed: {}", stderr.trim()),
            Some("extract_contracts".to_string()),
        ));
    }

    let contracts: FileContracts = serde_json::from_slice(&output.stdout).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse contract script output: {}", e),
            Some("extract_contracts".to_string()),
        )
    })?;

    Ok(Some(contracts))
}

/// Find an installed extension that handles a file extension and has scripts.contract.
fn find_extension_with_contract(file_ext: &str) -> Option<extension::ExtensionManifest> {
    extension::load_all_extensions().ok().and_then(|manifests| {
        manifests
            .into_iter()
            .find(|m| m.handles_file_extension(file_ext) && m.contract_script().is_some())
    })
}

// ── Utility methods ──

impl FunctionContract {
    /// Returns true if this function can fail (returns Result or Option).
    pub fn can_fail(&self) -> bool {
        matches!(
            self.signature.return_type,
            ReturnShape::ResultType { .. } | ReturnShape::OptionType { .. }
        )
    }

    /// Returns true if this function has side effects.
    pub fn has_effects(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Returns true if this function is pure (no effects, no mutation).
    pub fn is_pure(&self) -> bool {
        self.effects.is_empty()
            && self
                .signature
                .receiver
                .as_ref()
                .is_none_or(|r| !matches!(r, Receiver::MutRef))
            && !self.signature.params.iter().any(|p| p.mutable)
    }

    /// Count the number of distinct return paths.
    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }

    /// Group branches by return variant (ok/err/some/none/true/false).
    pub fn branches_by_variant(&self) -> HashMap<&str, Vec<&Branch>> {
        let mut map: HashMap<&str, Vec<&Branch>> = HashMap::new();
        for branch in &self.branches {
            map.entry(branch.returns.variant.as_str())
                .or_default()
                .push(branch);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contract() -> FunctionContract {
        FunctionContract {
            name: "validate_write".to_string(),
            file: "src/core/engine/validate_write.rs".to_string(),
            line: 86,
            signature: Signature {
                params: vec![
                    Param {
                        name: "root".to_string(),
                        param_type: "&Path".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                    Param {
                        name: "changed_files".to_string(),
                        param_type: "&[PathBuf]".to_string(),
                        mutable: false,
                        has_default: false,
                    },
                ],
                return_type: ReturnShape::ResultType {
                    ok_type: "ValidationResult".to_string(),
                    err_type: "Error".to_string(),
                },
                receiver: None,
                is_public: true,
                is_async: false,
                generics: vec![],
            },
            branches: vec![
                Branch {
                    condition: "changed_files.is_empty()".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("skipped".to_string()),
                    },
                    effects: vec![],
                    line: Some(91),
                },
                Branch {
                    condition: "validation command fails".to_string(),
                    returns: ReturnValue {
                        variant: "ok".to_string(),
                        value: Some("failed".to_string()),
                    },
                    effects: vec![
                        Effect::ProcessSpawn {
                            command: Some("sh".to_string()),
                        },
                        Effect::Mutation {
                            target: "rollback".to_string(),
                        },
                    ],
                    line: Some(130),
                },
            ],
            early_returns: 2,
            effects: vec![
                Effect::ProcessSpawn {
                    command: Some("sh".to_string()),
                },
                Effect::Mutation {
                    target: "rollback".to_string(),
                },
            ],
            calls: vec![
                FunctionCall {
                    function: "resolve_validate_command".to_string(),
                    forwards: vec!["root".to_string(), "changed_files".to_string()],
                },
                FunctionCall {
                    function: "Command::new".to_string(),
                    forwards: vec![],
                },
            ],
            impl_type: None,
        }
    }

    #[test]
    fn can_fail_returns_true_for_result() {
        let c = sample_contract();
        assert!(c.can_fail());
    }

    #[test]
    fn has_effects_returns_true() {
        let c = sample_contract();
        assert!(c.has_effects());
    }

    #[test]
    fn is_pure_returns_false_with_effects() {
        let c = sample_contract();
        assert!(!c.is_pure());
    }

    #[test]
    fn is_pure_returns_true_for_pure_function() {
        let mut c = sample_contract();
        c.effects.clear();
        for b in &mut c.branches {
            b.effects.clear();
        }
        assert!(c.is_pure());
    }

    #[test]
    fn branch_count() {
        let c = sample_contract();
        assert_eq!(c.branch_count(), 2);
    }

    #[test]
    fn branches_by_variant_groups_correctly() {
        let c = sample_contract();
        let grouped = c.branches_by_variant();
        assert_eq!(grouped.get("ok").unwrap().len(), 2);
        assert!(grouped.get("err").is_none());
    }

    #[test]
    fn contract_serializes_to_json() {
        let c = sample_contract();
        let json = serde_json::to_string_pretty(&c).unwrap();
        assert!(json.contains("validate_write"));
        assert!(json.contains("result"));
        assert!(json.contains("process_spawn"));
    }

    #[test]
    fn contract_roundtrips_through_json() {
        let c = sample_contract();
        let json = serde_json::to_string(&c).unwrap();
        let deserialized: FunctionContract = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "validate_write");
        assert_eq!(deserialized.branches.len(), 2);
        assert_eq!(deserialized.effects.len(), 2);
    }
}
