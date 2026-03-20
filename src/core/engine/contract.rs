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

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::error::{Error, Result};
use crate::extension;

// ── Core data types ──

/// A function's complete behavioral contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionContract {
    /// Function name.
    pub name: String,
    /// File path relative to component root.
    pub file: String,
    /// 1-indexed line number of the function declaration.
    pub line: usize,
    /// Function signature.
    pub signature: Signature,
    /// Distinct return paths through the function.
    pub branches: Vec<Branch>,
    /// Number of early return / guard clause statements.
    pub early_returns: usize,
    /// Aggregate side effects across all branches.
    pub effects: Vec<Effect>,
    /// Functions called within this function.
    pub calls: Vec<FunctionCall>,
    /// The type this method belongs to (from the impl block).
    /// `None` for free functions. `Some("Foo")` for `impl Foo { fn bar(&self) }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impl_type: Option<String>,
}

/// Function signature: params, return type, receiver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    /// Positional parameters (excludes self/receiver).
    pub params: Vec<Param>,
    /// Return type shape.
    pub return_type: ReturnShape,
    /// Receiver (self, &self, &mut self) if this is a method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<Receiver>,
    /// Whether the function is public.
    #[serde(default)]
    pub is_public: bool,
    /// Whether the function is async.
    #[serde(default)]
    pub is_async: bool,
    /// Generic type parameters, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generics: Vec<String>,
}

/// A function parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    /// Parameter name.
    pub name: String,
    /// Type as written in source (language-specific syntax).
    #[serde(rename = "type")]
    pub param_type: String,
    /// Whether the parameter is mutable (&mut in Rust, & in PHP by-ref).
    #[serde(default)]
    pub mutable: bool,
    /// Whether the parameter has a default value.
    #[serde(default)]
    pub has_default: bool,
}

/// Return type shape — the structural pattern of what a function returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "shape")]
pub enum ReturnShape {
    /// Returns nothing (void / unit).
    #[serde(rename = "unit")]
    Unit,
    /// Returns a simple value (not Result/Option/bool).
    #[serde(rename = "value")]
    Value {
        /// Type as written in source.
        #[serde(rename = "type")]
        value_type: String,
    },
    /// Returns bool.
    #[serde(rename = "bool")]
    Bool,
    /// Returns Option<T>.
    #[serde(rename = "option")]
    OptionType {
        /// The inner type T.
        some_type: String,
    },
    /// Returns Result<T, E>.
    #[serde(rename = "result")]
    ResultType {
        /// The success type T.
        ok_type: String,
        /// The error type E.
        err_type: String,
    },
    /// Returns a collection (Vec<T>, Iterator, etc).
    #[serde(rename = "collection")]
    Collection {
        /// Element type.
        element_type: String,
    },
    /// Unrecognized return type — raw string.
    #[serde(rename = "unknown")]
    Unknown {
        /// Raw return type text.
        raw: String,
    },
}

/// Method receiver type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Receiver {
    /// `self` — takes ownership.
    OwnedSelf,
    /// `&self` — immutable borrow.
    Ref,
    /// `&mut self` — mutable borrow.
    MutRef,
}

// ── Control flow ──

/// A distinct execution path through the function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    /// Human-readable description of the condition that triggers this branch.
    pub condition: String,
    /// What this branch returns.
    pub returns: ReturnValue,
    /// Side effects specific to this branch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    /// Line number where this branch starts (1-indexed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

/// What a branch returns — the variant + value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnValue {
    /// The variant of the return type this branch produces.
    /// For Result: "ok" or "err". For Option: "some" or "none".
    /// For bool: "true" or "false". For value: "value".
    pub variant: String,
    /// A description of the returned value (e.g., "skipped", "passed", "empty vec").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

// ── Effects ──

/// A side effect that a function may perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Effect {
    /// Reads from the filesystem.
    #[serde(rename = "file_read")]
    FileRead,
    /// Writes to the filesystem.
    #[serde(rename = "file_write")]
    FileWrite,
    /// Deletes files.
    #[serde(rename = "file_delete")]
    FileDelete,
    /// Spawns a subprocess.
    #[serde(rename = "process_spawn")]
    ProcessSpawn {
        /// The command being spawned, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    /// Mutates a parameter or self.
    #[serde(rename = "mutation")]
    Mutation {
        /// What is being mutated (e.g., "self.field", "rollback", "param_name").
        target: String,
    },
    /// Can panic (panic!, unreachable!, todo!, unwrap, expect).
    #[serde(rename = "panic")]
    Panic {
        /// The panic message or expression, if extractable.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Performs network I/O.
    #[serde(rename = "network")]
    Network,
    /// Allocates resources that need cleanup (tempfiles, locks, etc).
    #[serde(rename = "resource_alloc")]
    ResourceAlloc {
        /// Description of the resource.
        #[serde(skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
    },
    /// Logs or prints output.
    #[serde(rename = "logging")]
    Logging,
}

// ── Dependencies ──

/// A function call made within the analyzed function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// The called function name (may include module path).
    pub function: String,
    /// Parameters from the outer function that are forwarded to this call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<String>,
}

// ── File-level container ──

/// All contracts extracted from a single source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContracts {
    /// Relative file path.
    pub file: String,
    /// Extracted function contracts.
    pub contracts: Vec<FunctionContract>,
}

// ── Type definitions ──

/// A type definition extracted from source code (struct, enum, class).
///
/// Language-agnostic representation of a type's structure. Used by the test
/// generator to resolve return types to their fields, enabling field-level
/// assertions instead of opaque `let _ = inner;` placeholders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDefinition {
    /// Type name (e.g., "ValidationResult", "Config").
    pub name: String,
    /// Kind: "struct", "enum", "class".
    pub kind: String,
    /// File where this type is defined (relative path).
    pub file: String,
    /// 1-indexed line number of the definition.
    pub line: usize,
    /// Fields/properties of this type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
    /// Whether the type is public.
    #[serde(default)]
    pub is_public: bool,
}

/// A single field/property within a type definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    /// Field name.
    pub name: String,
    /// Field type as written in source.
    #[serde(rename = "type")]
    pub field_type: String,
    /// Whether the field is public.
    #[serde(default)]
    pub is_public: bool,
}

/// Parse field definitions from a struct/class source body using a regex pattern.
///
/// The `field_pattern` is a regex with capture groups for field name and type.
/// `name_group` and `type_group` specify which capture groups to use (1-indexed).
///
/// `visibility_pattern` optionally matches a visibility prefix (e.g., `pub`).
///
/// This is language-agnostic: the grammar provides the regex patterns and
/// capture group assignments.
pub fn parse_fields_from_source(
    source: &str,
    field_pattern: &str,
    visibility_pattern: Option<&str>,
    name_group: usize,
    type_group: usize,
) -> Vec<FieldDef> {
    let field_re = match regex::Regex::new(field_pattern) {
        Ok(re) => re,
        Err(_) => return vec![],
    };
    let vis_re = visibility_pattern.and_then(|p| regex::Regex::new(p).ok());

    let mut fields = Vec::new();
    // Skip the first line (struct declaration) and last line (closing brace)
    let lines: Vec<&str> = source.lines().collect();
    for line in &lines {
        let trimmed = line.trim();
        // Skip empty lines, comments, attributes
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with('{')
            || trimmed == "}"
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }
        // Skip the struct/class declaration line itself
        if trimmed.contains("struct ") || trimmed.contains("class ") || trimmed.contains("enum ") {
            continue;
        }

        if let Some(caps) = field_re.captures(trimmed) {
            let name = caps
                .get(name_group)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let field_type = caps
                .get(type_group)
                .map(|m| m.as_str().trim_end_matches(',').trim().to_string())
                .unwrap_or_default();

            if name.is_empty() || field_type.is_empty() {
                continue;
            }

            let is_public = vis_re
                .as_ref()
                .map(|re| re.is_match(trimmed))
                .unwrap_or(false);

            fields.push(FieldDef {
                name,
                field_type,
                is_public,
            });
        }
    }

    fields
}

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
