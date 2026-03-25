//! Surface compiler warnings (dead code, unused imports, unused variables) as audit findings.
//!
//! Runs the project's compiler/checker and parses structured output into audit findings.
//! For Rust: `cargo check --message-format=json`
//! For TypeScript: `tsc --noEmit` (future)
//! For Go: `go vet ./...` (future)
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/636

use std::path::Path;

use super::{AuditFinding, Finding, Severity};

/// A parsed compiler warning.
#[derive(Debug, Clone)]
struct CompilerWarning {
    /// Compiler warning code (e.g., "dead_code", "unused_imports").
    code: String,
    /// Human-readable message.
    message: String,
    /// Relative file path from the project root.
    file: String,
    /// 1-indexed line number.
    line: usize,
}

/// Run compiler checks and return findings for any warnings detected.
pub fn run(root: &Path) -> Vec<Finding> {
    if root.join("Cargo.toml").exists() {
        run_cargo_check(root)
    } else {
        // Future: TypeScript, Go, etc.
        Vec::new()
    }
}

/// Run `cargo check --message-format=json` and parse warnings into findings.
fn run_cargo_check(root: &Path) -> Vec<Finding> {
    let output = match std::process::Command::new("cargo")
        .args(["check", "--message-format=json"])
        .current_dir(root)
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            crate::log_status!(
                "audit",
                "Could not run cargo check: {} — skipping compiler warnings",
                e
            );
            return Vec::new();
        }
    };

    // cargo check outputs one JSON object per line on stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let warnings = parse_cargo_json_output(&stdout, root);

    warnings
        .into_iter()
        .map(|w| Finding {
            file: w.file.clone(),
            kind: AuditFinding::CompilerWarning,
            severity: Severity::Warning,
            convention: "compiler".to_string(),
            description: format!("[{}] {}", w.code, w.message),
            suggestion: suggestion_for_code(&w.code, &w.file, w.line),
        })
        .collect()
}

/// Parse `cargo check --message-format=json` output into structured warnings.
fn parse_cargo_json_output(stdout: &str, root: &Path) -> Vec<CompilerWarning> {
    let root_str = root.to_string_lossy();
    let mut warnings = Vec::new();

    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if msg.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
            continue;
        }

        let Some(message) = msg.get("message") else {
            continue;
        };

        if message.get("level").and_then(|v| v.as_str()) != Some("warning") {
            continue;
        }

        let code = message
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|c| c.as_str())
            .unwrap_or("unknown")
            .to_string();

        let text = message
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        // Find the primary span
        let spans = message
            .get("spans")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();

        let primary = spans
            .iter()
            .find(|s| s.get("is_primary").and_then(|v| v.as_bool()) == Some(true))
            .or_else(|| spans.first());

        let (file, line_num) = if let Some(span) = primary {
            let file_name = span
                .get("file_name")
                .and_then(|f| f.as_str())
                .unwrap_or("")
                .to_string();
            let line_start = span.get("line_start").and_then(|l| l.as_u64()).unwrap_or(0) as usize;

            // Make path relative to root
            let relative = file_name
                .strip_prefix(&*root_str)
                .map(|s| s.trim_start_matches('/').to_string())
                .unwrap_or(file_name);

            (relative, line_start)
        } else {
            (String::new(), 0)
        };

        // Skip warnings from dependencies or build scripts
        if file.is_empty() || file.starts_with('/') || file.contains("/.cargo/") {
            continue;
        }

        // Skip certain noise warnings that aren't actionable
        if code == "unknown" && text.contains("generated") {
            continue;
        }

        warnings.push(CompilerWarning {
            code,
            message: text,
            file,
            line: line_num,
        });
    }

    // Deduplicate — cargo can emit the same warning multiple times for different targets
    warnings.sort_by(|a, b| (&a.file, a.line, &a.code).cmp(&(&b.file, b.line, &b.code)));
    warnings.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.code == b.code);

    warnings
}

/// Generate a fix suggestion based on the warning code.
fn suggestion_for_code(code: &str, _file: &str, _line: usize) -> String {
    match code {
        "dead_code" => {
            "Remove the unused item or add `#[allow(dead_code)]` if intentionally reserved"
                .to_string()
        }
        "unused_imports" => "Remove the unused import".to_string(),
        "unused_variables" => "Prefix with underscore `_` or remove the variable".to_string(),
        "unused_assignments" => "Remove the unnecessary assignment".to_string(),
        "unused_mut" => "Remove the `mut` qualifier".to_string(),
        "unreachable_code" => "Remove or refactor the unreachable code path".to_string(),
        "unused_must_use" => {
            "Handle the return value or explicitly ignore with `let _ = ...`".to_string()
        }
        _ => format!("Address compiler warning: {}", code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_cargo_json_output_extracts_warnings() {
        let root = Path::new("/project");
        let json_lines = r#"{"reason":"compiler-artifact","package_id":"foo 0.1.0","target":{"name":"foo"}}
{"reason":"compiler-message","package_id":"foo 0.1.0","message":{"rendered":"warning: unused import","level":"warning","code":{"code":"unused_imports","explanation":null},"message":"unused import: `std::fs`","spans":[{"file_name":"src/main.rs","byte_start":0,"byte_end":10,"line_start":3,"line_end":3,"column_start":5,"column_end":15,"is_primary":true,"text":[]}]}}
{"reason":"compiler-message","package_id":"foo 0.1.0","message":{"rendered":"warning: function `old` is never used","level":"warning","code":{"code":"dead_code","explanation":null},"message":"function `old` is never used","spans":[{"file_name":"src/lib.rs","byte_start":50,"byte_end":60,"line_start":10,"line_end":10,"column_start":8,"column_end":11,"is_primary":true,"text":[]}]}}
{"reason":"build-finished","success":true}"#;

        let warnings = parse_cargo_json_output(json_lines, root);
        assert_eq!(warnings.len(), 2);
        // Sorted by (file, line, code) — src/lib.rs comes before src/main.rs
        assert_eq!(warnings[0].code, "dead_code");
        assert_eq!(warnings[0].file, "src/lib.rs");
        assert_eq!(warnings[0].line, 10);
        assert_eq!(warnings[1].code, "unused_imports");
        assert_eq!(warnings[1].file, "src/main.rs");
        assert_eq!(warnings[1].line, 3);
    }

    #[test]
    fn run_on_rust_project_returns_findings() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();

        // Create a minimal Rust project with a dead code warning
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"test-warn\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn unused_function() {}\npub fn used() {}\n",
        )
        .unwrap();

        let findings = run(root);
        // Should find at least the dead_code warning for unused_function
        assert!(
            findings.iter().any(|f| f.description.contains("dead_code")
                || f.description.contains("unused")
                || f.description.contains("never used")),
            "expected dead_code warning, got: {:?}",
            findings
        );
    }

    #[test]
    fn test_run_default_path() {
        let root = Path::new("");
        let result = run(&root);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

}
