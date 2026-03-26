//! Refactor add — apply fixes from audit results or explicit additions.
//!
//! Two modes:
//! - **From audit**: Parse saved audit JSON, generate fixes via `refactor::plan::generate_audit_fixes()`,
//!   optionally apply. Composable pipeline step: `audit > audit.json && refactor add --from-audit @audit.json`
//! - **Explicit**: Add imports/stubs to files matching a glob pattern.
//!   Example: `refactor add --import "use serde::Serialize" --to "src/commands/*.rs"`

use std::path::{Path, PathBuf};

use crate::code_audit::CodeAuditResult;
use crate::refactor::auto::{self, Fix, FixResult, Insertion, InsertionKind};
use crate::refactor::plan;
use crate::{component, Result};

/// Result of an explicit import addition (not from audit).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AddResult {
    /// Files that were modified (or would be in dry-run).
    pub fixes: Vec<Fix>,
    /// Total number of insertions planned.
    pub total_insertions: usize,
    /// Number of files actually written (0 in dry-run).
    pub files_modified: usize,
}

// ============================================================================
// From Audit Mode
// ============================================================================

/// Generate fixes from a deserialized audit result.
///
/// This is the composable version of `audit --fix`: parse saved audit JSON
/// and generate the same fixes that `audit --fix` would produce.
pub fn fixes_from_audit(audit: &CodeAuditResult, write: bool) -> Result<FixResult> {
    let root = Path::new(&audit.source_path);

    if !root.is_dir() {
        return Err(crate::Error::validation_invalid_argument(
            "from-audit",
            format!(
                "Audit source_path '{}' is not a directory on this machine. \
                 Run from the same machine where the audit was performed.",
                audit.source_path
            ),
            None,
            None,
        ));
    }

    let mut fix_result = plan::generate_audit_fixes(audit, root, &auto::FixPolicy::default());

    if write && !fix_result.fixes.is_empty() {
        let applied = auto::apply_fixes(&mut fix_result.fixes, root);
        fix_result.files_modified = applied;
    }

    Ok(fix_result)
}

// ============================================================================
// Explicit Add Mode
// ============================================================================

/// Add an import statement to files matching a glob or path pattern.
///
/// The `import_line` is the exact code to add (e.g., `use serde::Serialize;`).
/// The `target` is a glob pattern or directory path resolved against the root.
pub fn add_import(
    import_line: &str,
    target: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> Result<AddResult> {
    let root = resolve_root(component_id, path)?;
    let matched_files = resolve_target_files(&root, target)?;

    if matched_files.is_empty() {
        return Err(crate::Error::validation_invalid_argument(
            "to",
            format!("No files matched pattern '{}'", target),
            None,
            Some(vec![
                "Use a glob pattern: --to \"src/**/*.rs\"".to_string(),
                "Use a relative path: --to src/commands/refactor.rs".to_string(),
            ]),
        ));
    }

    let mut fixes: Vec<Fix> = Vec::new();

    for file_path in &matched_files {
        let abs_path = root.join(file_path);
        let content = std::fs::read_to_string(&abs_path).map_err(|e| {
            crate::Error::internal_io(e.to_string(), Some(format!("read {}", file_path)))
        })?;

        // Skip if the import already exists in the file
        if content.contains(import_line.trim()) {
            continue;
        }

        fixes.push(Fix {
            file: file_path.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![Insertion {
                primitive: None,
                kind: InsertionKind::ImportAdd,
                finding: crate::code_audit::AuditFinding::MissingImport,
                manual_only: false,
                auto_apply: false,
                blocked_reason: None,
                code: import_line.trim().to_string(),
                description: format!("Add import: {}", import_line.trim()),
            }],
            applied: false,
        });
    }

    let total_insertions = fixes.len();
    let mut files_modified = 0;

    if write && !fixes.is_empty() {
        files_modified = auto::apply_fixes(&mut fixes, &root);
    }

    Ok(AddResult {
        fixes,
        total_insertions,
        files_modified,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Resolve the root directory from component ID or explicit path.
fn resolve_root(component_id: Option<&str>, path: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = path {
        let pb = PathBuf::from(p);
        if !pb.is_dir() {
            return Err(crate::Error::validation_invalid_argument(
                "path",
                format!("Not a directory: {}", p),
                None,
                None,
            ));
        }
        Ok(pb)
    } else {
        let comp = component::resolve(component_id)?;
        let validated = component::validate_local_path(&comp)?;
        Ok(validated)
    }
}

/// Resolve target files from a glob pattern or direct file path.
///
/// Returns relative paths from root.
fn resolve_target_files(root: &Path, target: &str) -> Result<Vec<String>> {
    let abs_target = root.join(target);

    // If it's a direct file path, return it
    if abs_target.is_file() {
        let rel = abs_target
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| target.to_string());
        return Ok(vec![rel]);
    }

    // Try as a glob pattern
    let glob_pattern = root.join(target).to_string_lossy().to_string();
    let entries: Vec<String> = glob::glob(&glob_pattern)
        .map_err(|e| {
            crate::Error::validation_invalid_argument(
                "to",
                format!("Invalid glob pattern '{}': {}", target, e),
                None,
                None,
            )
        })?
        .filter_map(|entry| entry.ok())
        .filter(|p| p.is_file())
        .filter_map(|p| {
            p.strip_prefix(root)
                .ok()
                .map(|rel| rel.to_string_lossy().to_string())
        })
        .collect();

    Ok(entries)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_import_to_matching_files() {
        let dir = std::env::temp_dir().join("homeboy_refactor_add_test");
        let src = dir.join("src");
        let _ = std::fs::create_dir_all(&src);

        std::fs::write(src.join("one.rs"), "use std::path::Path;\n\nfn main() {}\n").unwrap();

        std::fs::write(src.join("two.rs"), "use std::io;\n\nfn helper() {}\n").unwrap();

        // File that already has the import
        std::fs::write(
            src.join("three.rs"),
            "use serde::Serialize;\n\nfn other() {}\n",
        )
        .unwrap();

        let result = add_import(
            "use serde::Serialize;",
            "src/*.rs",
            None,
            Some(dir.to_str().unwrap()),
            false,
        )
        .unwrap();

        // Should generate fixes for one.rs and two.rs, but skip three.rs
        assert_eq!(result.total_insertions, 2);
        assert_eq!(result.files_modified, 0); // dry run

        let fixed_files: Vec<&str> = result.fixes.iter().map(|f| f.file.as_str()).collect();
        assert!(fixed_files.contains(&"src/one.rs"));
        assert!(fixed_files.contains(&"src/two.rs"));
        assert!(!fixed_files.contains(&"src/three.rs"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_import_with_write() {
        let dir = std::env::temp_dir().join("homeboy_refactor_add_write_test");
        let src = dir.join("src");
        let _ = std::fs::create_dir_all(&src);

        std::fs::write(
            src.join("target.rs"),
            "use std::path::Path;\n\nfn main() {}\n",
        )
        .unwrap();

        let result = add_import(
            "use serde::Serialize;",
            "src/target.rs",
            None,
            Some(dir.to_str().unwrap()),
            true,
        )
        .unwrap();

        assert_eq!(result.total_insertions, 1);
        assert_eq!(result.files_modified, 1);

        let content = std::fs::read_to_string(src.join("target.rs")).unwrap();
        assert!(content.contains("use serde::Serialize;"));
        assert!(content.contains("use std::path::Path;")); // preserved

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_import_skips_existing() {
        let dir = std::env::temp_dir().join("homeboy_refactor_add_skip_test");
        let _ = std::fs::create_dir_all(&dir);

        std::fs::write(
            dir.join("existing.rs"),
            "use serde::Serialize;\n\nfn main() {}\n",
        )
        .unwrap();

        let result = add_import(
            "use serde::Serialize;",
            "existing.rs",
            None,
            Some(dir.to_str().unwrap()),
            false,
        )
        .unwrap();

        assert_eq!(result.total_insertions, 0);
        assert!(result.fixes.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_import_no_match_returns_error() {
        let dir = std::env::temp_dir().join("homeboy_refactor_add_nomatch_test");
        let _ = std::fs::create_dir_all(&dir);

        let result = add_import(
            "use serde::Serialize;",
            "nonexistent/*.rs",
            None,
            Some(dir.to_str().unwrap()),
            false,
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No files matched"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixes_from_audit_validates_source_path() {
        let audit = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: "/nonexistent/path/that/should/not/exist".to_string(),
            summary: crate::code_audit::AuditSummary {
                files_scanned: 0,
                conventions_detected: 0,
                outliers_found: 0,
                alignment_score: Some(1.0),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![],
            duplicate_groups: vec![],
        };

        let result = fixes_from_audit(&audit, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }
}
