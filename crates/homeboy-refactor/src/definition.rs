//! Language-generic struct/item definition discovery.
//!
//! Finding *where* a struct is defined and extracting its source block is
//! language-specific analysis — `pub struct Name { .. }` is Rust syntax, a PHP
//! class or a Go type looks nothing like it. So that analysis does **not** live
//! here. This module owns only the language-agnostic parts: walking candidate
//! files (by the extensions a refactor extension handles) and asking each
//! language extension, via the `find_definition` command, whether a given file
//! defines the named item and what its source block is.
//!
//! The Rust extension answers `find_definition` by matching `pub struct Name`
//! and brace-balancing the body; a PHP/Go/Swift extension answers the same
//! command with its own class/type syntax. Nothing in this file is
//! Rust-specific — that was previously not true (`homeboy-refactor` hardcoded
//! `format!("pub struct {} ", ..)`), which this module removes so the refactor
//! engine stays language-agnostic end to end.

use std::path::{Path, PathBuf};

use crate::refactor_provider::{invoke_refactor_value, refactor_providers, RefactorProvider};
use homeboy_core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use homeboy_core::Error;

/// A located definition: the file it lives in and its extracted source block.
pub(crate) struct LocatedDefinition {
    /// Path to the file containing the definition, relative to `root` when the
    /// scan produced a path under `root` (absolute otherwise).
    pub file: PathBuf,
    /// The full source block of the definition (attributes/doc comments +
    /// braced body), as extracted by the owning language extension.
    pub source: String,
}

/// Enumerate installed extensions that advertise a refactor script and handle at
/// least one file extension. Shared by definition-finding and the
/// propagate/collapse scans so they agree on which languages participate.
pub(crate) fn refactor_capable_extensions() -> Result<Vec<RefactorProvider>, Error> {
    let exts = refactor_providers();

    if exts.is_empty() {
        return Err(Error::validation_invalid_argument(
            "extension",
            "No extension with refactor capability found. Install a language refactor extension (e.g. the Rust extension).",
            None,
            None,
        ));
    }
    Ok(exts)
}

/// Extract a definition's source block from a file whose content is already in
/// hand, by asking each refactor extension's `find_definition` command. Returns
/// `None` if no extension recognizes the item in this content.
///
/// Used when the caller supplied an explicit `--definition` file: the file is
/// known, but the *source block* extraction is still language-specific.
pub(crate) fn extract_definition_source(
    struct_name: &str,
    file_content: &str,
    file_relative: &str,
    root: &Path,
    exts: &[RefactorProvider],
) -> Option<String> {
    if !file_content.contains(struct_name) {
        return None;
    }
    let file_extension = Path::new(file_relative).extension()?.to_str()?;
    for ext_manifest in exts {
        if !ext_manifest.handles_file_extension(file_extension) {
            continue;
        }
        let cmd = serde_json::json!({
            "command": "find_definition",
            "struct_name": struct_name,
            "file_content": file_content,
            "file_path": file_relative,
        });
        let Some(result) = invoke_refactor_value(ext_manifest, root, file_extension, cmd) else {
            continue;
        };
        let defines = result
            .get("defines")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if defines {
            if let Some(source) = result.get("struct_source").and_then(|v| v.as_str()) {
                return Some(source.to_string());
            }
        }
    }
    None
}

/// Find the file that defines `struct_name` by scanning the codebase and asking
/// each refactor extension's `find_definition` command. Returns both the file
/// path and the extracted source block (so callers need not re-read/re-parse).
///
/// The file walk is language-agnostic (driven by the extensions each refactor
/// extension handles); the definition recognition is delegated to the language
/// extension.
pub(crate) fn find_definition(
    struct_name: &str,
    root: &Path,
    exts: &[RefactorProvider],
) -> Result<LocatedDefinition, Error> {
    for ext_manifest in exts {
        let handled_exts = ext_manifest.file_extensions.clone();
        let scan_config = ScanConfig {
            extensions: ExtensionFilter::Only(handled_exts.clone()),
            skip_hidden: true,
            ..Default::default()
        };
        let files = codebase_scan::walk_files(root, &scan_config);

        for file_path in &files {
            let Ok(content) = std::fs::read_to_string(file_path) else {
                continue;
            };
            // Cheap pre-filter before paying for a script invocation.
            if !content.contains(struct_name) {
                continue;
            }
            let relative = file_path
                .strip_prefix(root)
                .unwrap_or(file_path)
                .to_string_lossy()
                .to_string();
            let Some(file_extension) = file_path.extension().and_then(|value| value.to_str())
            else {
                continue;
            };

            let cmd = serde_json::json!({
                "command": "find_definition",
                "struct_name": struct_name,
                "file_content": content,
                "file_path": relative,
            });
            let Some(result) = invoke_refactor_value(ext_manifest, root, file_extension, cmd)
            else {
                continue;
            };
            let defines = result
                .get("defines")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !defines {
                continue;
            }
            let Some(source) = result.get("struct_source").and_then(|v| v.as_str()) else {
                continue;
            };
            return Ok(LocatedDefinition {
                file: file_path.clone(),
                source: source.to_string(),
            });
        }
    }

    Err(Error::validation_invalid_argument(
        "struct_name",
        format!(
            "Could not find a definition for `{}` under {}",
            struct_name,
            root.display()
        ),
        None,
        Some(vec![format!(
            "homeboy refactor propagate --struct-name {} --definition src/path/to/file.rs",
            struct_name
        )]),
    ))
}
