use super::manifest::ExtensionManifest;

// ============================================================================
// Refactor Script Protocol
// ============================================================================

/// Run a extension's refactor script with a command.
///
/// The script receives a JSON command on stdin and outputs JSON on stdout.
/// Commands are dispatched by the `command` field. Each command has its own
/// input/output schema.
///
/// Supported commands:
/// - `parse_items`: Parse source file, return all top-level items with boundaries
/// - `resolve_imports`: Given moved items, resolve what imports the destination needs
/// - `adjust_visibility`: Adjust visibility of items crossing module boundaries
/// - `find_related_tests`: Find test functions related to named items
/// - `rewrite_import_path`: Compute the corrected import path for a moved item
pub fn run_refactor_script(
    extension: &ExtensionManifest,
    command: &serde_json::Value,
) -> Option<serde_json::Value> {
    let extension_path = extension.extension_path.as_deref()?;
    let script_rel = extension.refactor_script()?;
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return None;
    }

    // Invoke the script directly so its shebang resolves the interpreter.
    // Wrapping with `sh -c <script>` bypasses `#!/usr/bin/env bash` and runs
    // under POSIX sh — which breaks scripts using bash-only features. See #1276.
    let output = std::process::Command::new(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(command.to_string().as_bytes());
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            crate::log_status!("refactor", "Extension script error: {}", stderr.trim());
        }
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
}

/// Output from a `parse_items` refactor command.
/// Each item has boundaries, kind, name, visibility, and source text.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedItem {
    /// Name of the item (function, struct, etc.).
    pub name: String,
    /// What kind of item (function, struct, enum, const, etc.).
    pub kind: String,
    /// Start line (1-indexed, includes doc comments and attributes).
    pub start_line: usize,
    /// End line (1-indexed, inclusive).
    pub end_line: usize,
    /// The extracted source code (including doc comments and attributes).
    pub source: String,
    /// Visibility: "pub", "pub(crate)", "pub(super)", or "" for private.
    #[serde(default)]
    pub visibility: String,
}

impl From<crate::core::extension::grammar_items::GrammarItem> for ParsedItem {
    fn from(gi: crate::core::extension::grammar_items::GrammarItem) -> Self {
        Self {
            name: gi.name,
            kind: gi.kind,
            start_line: gi.start_line,
            end_line: gi.end_line,
            source: gi.source,
            visibility: gi.visibility,
        }
    }
}

/// Output from a `resolve_imports` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedImports {
    /// Import statements needed in the destination file.
    pub needed_imports: Vec<String>,
    /// Warnings about imports that couldn't be resolved.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Output from a `find_related_tests` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelatedTests {
    /// Test items that should move with the extracted items.
    pub tests: Vec<ParsedItem>,
    /// Names of tests that reference multiple moved/unmoved items (can't cleanly move).
    #[serde(default)]
    pub ambiguous: Vec<String>,
}

/// Output from an `adjust_visibility` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdjustedItem {
    /// The item source with visibility adjusted.
    pub source: String,
    /// Whether visibility was changed.
    pub changed: bool,
    /// Original visibility.
    pub original_visibility: String,
    /// New visibility.
    pub new_visibility: String,
}

/// Output from a `rewrite_import_path` refactor command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RewrittenImport {
    /// Original import path.
    pub original: String,
    /// Corrected import path.
    pub rewritten: String,
    /// Whether the path changed.
    pub changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_refactor_script() {
        let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": "Example",
            "version": "0.0.0"
        }))
        .unwrap();
        assert!(run_refactor_script(&manifest, &serde_json::json!({})).is_none());
    }

    #[test]
    fn test_parsed_item_from_grammar_item() {
        let item = crate::core::extension::grammar_items::GrammarItem {
            name: "run".to_string(),
            kind: "function".to_string(),
            start_line: 3,
            end_line: 7,
            source: "fn run() {}".to_string(),
            visibility: "pub".to_string(),
        };

        let parsed = ParsedItem::from(item);

        assert_eq!(parsed.name, "run");
        assert_eq!(parsed.kind, "function");
        assert_eq!(parsed.start_line, 3);
        assert_eq!(parsed.end_line, 7);
        assert_eq!(parsed.source, "fn run() {}");
        assert_eq!(parsed.visibility, "pub");
    }
}
