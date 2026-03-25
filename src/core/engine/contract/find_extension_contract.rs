//! find_extension_contract — extracted from contract.rs.

use std::path::Path;
use crate::code_audit::core_fingerprint::load_grammar_for_ext;
use crate::error::{Error, Result};
use crate::extension;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use super::FileContracts;
use super::FieldDef;


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
pub(crate) fn find_extension_with_contract(file_ext: &str) -> Option<extension::ExtensionManifest> {
    extension::load_all_extensions().ok().and_then(|manifests| {
        manifests
            .into_iter()
            .find(|m| m.handles_file_extension(file_ext) && m.contract_script().is_some())
    })
}
