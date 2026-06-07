use super::manifest::ExtensionManifest;
use crate::core::engine::command::{wait_with_bounded_output, DEFAULT_CAPTURE_LIMIT_BYTES};

#[derive(Debug, Clone)]
pub struct RefactorScriptFailure {
    pub kind: RefactorScriptFailureKind,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub parsed_stdout: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefactorScriptFailureKind {
    MissingScript,
    SpawnFailed,
    NonZeroExit,
    InvalidJson,
}

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
    match run_refactor_script_result(extension, command) {
        Ok(value) => Some(value),
        Err(failure) => {
            if failure.kind == RefactorScriptFailureKind::NonZeroExit && !failure.stderr.is_empty()
            {
                crate::log_status!(
                    "refactor",
                    "Extension script error: {}",
                    failure.stderr.trim()
                );
            }
            None
        }
    }
}

pub fn run_refactor_script_result(
    extension: &ExtensionManifest,
    command: &serde_json::Value,
) -> Result<serde_json::Value, RefactorScriptFailure> {
    let Some(extension_path) = extension.extension_path.as_deref() else {
        return Err(RefactorScriptFailure::new(
            RefactorScriptFailureKind::MissingScript,
        ));
    };
    let Some(script_rel) = extension.refactor_script() else {
        return Err(RefactorScriptFailure::new(
            RefactorScriptFailureKind::MissingScript,
        ));
    };
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return Err(RefactorScriptFailure::new(
            RefactorScriptFailureKind::MissingScript,
        ));
    }

    // Invoke the script directly so its shebang resolves the interpreter.
    // Wrapping with `sh -c <script>` bypasses `#!/usr/bin/env bash` and runs
    // under POSIX sh — which breaks scripts using bash-only features. See #1276.
    let mut child =
        spawn_refactor_script(&script_path).map_err(RefactorScriptFailure::spawn_failed)?;
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(command.to_string().as_bytes());
    }
    let output = wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES)
        .map_err(RefactorScriptFailure::spawn_failed)?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let parsed_stdout = serde_json::from_str(&stdout).ok();

    if !output.status.success() {
        return Err(RefactorScriptFailure {
            kind: RefactorScriptFailureKind::NonZeroExit,
            exit_code: output.status.code(),
            stdout,
            stderr,
            parsed_stdout,
        });
    }

    parsed_stdout.ok_or_else(|| RefactorScriptFailure {
        kind: RefactorScriptFailureKind::InvalidJson,
        exit_code: output.status.code(),
        stdout,
        stderr,
        parsed_stdout: None,
    })
}

fn spawn_refactor_script(script_path: &std::path::Path) -> std::io::Result<std::process::Child> {
    let mut last_error = None;

    for attempt in 0..3 {
        match std::process::Command::new(script_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) if is_transient_spawn_error(&error) && attempt < 2 => {
                last_error = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(25 * (attempt + 1)));
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.expect("transient spawn error captured before retry"))
}

fn is_transient_spawn_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(11) | Some(26))
}

impl RefactorScriptFailure {
    fn spawn_failed(error: std::io::Error) -> Self {
        Self {
            kind: RefactorScriptFailureKind::SpawnFailed,
            exit_code: None,
            stdout: String::new(),
            stderr: error.to_string(),
            parsed_stdout: None,
        }
    }

    fn new(kind: RefactorScriptFailureKind) -> Self {
        Self {
            kind,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            parsed_stdout: None,
        }
    }
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
