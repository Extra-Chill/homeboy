//! fingerprint — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;

use super::conventions::Language;

/// A structural fingerprint extracted from a single source file.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileFingerprint {
    /// Path relative to component root.
    pub relative_path: String,
    /// Language detected from extension.
    pub language: Language,
    /// Method/function names found in the file.
    pub methods: Vec<String>,
    /// Registration calls found (e.g., add_action, register_rest_route).
    pub registrations: Vec<String>,
    /// Class or struct name if found.
    pub type_name: Option<String>,
    /// Interfaces or traits implemented.
    pub implements: Vec<String>,
    /// Namespace declaration (PHP namespace, Rust mod path).
    pub namespace: Option<String>,
    /// Import/use statements.
    pub imports: Vec<String>,
    /// Raw file content (for import usage analysis).
    pub content: String,
    /// Method name → normalized body hash for duplication detection.
    /// Populated by extension scripts that support it; empty otherwise.
    pub method_hashes: HashMap<String, String>,
    /// Method name → structural hash for near-duplicate detection.
    /// Identifiers/literals replaced with positional tokens before hashing.
    /// Populated by extension scripts that support it; empty otherwise.
    pub structural_hashes: HashMap<String, String>,
}

/// Extract a structural fingerprint from a source file.
///
/// Dispatches to an installed extension extension that handles the file's extension
/// and has a fingerprint script configured. No extension = no fingerprint.
pub fn fingerprint_file(path: &Path, root: &Path) -> Option<FileFingerprint> {
    use crate::extension;

    let ext = path.extension()?.to_str()?;
    let content = std::fs::read_to_string(path).ok()?;
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    let matched_extension = extension::find_extension_for_file_ext(ext, "fingerprint")?;
    let output = extension::run_fingerprint_script(&matched_extension, &relative_path, &content)?;

    let language = Language::from_extension(ext);

    Some(FileFingerprint {
        relative_path,
        language,
        methods: output.methods,
        registrations: output.registrations,
        type_name: output.type_name,
        implements: output.implements,
        namespace: output.namespace,
        imports: output.imports,
        content,
        method_hashes: output.method_hashes,
        structural_hashes: output.structural_hashes,
    })
}
