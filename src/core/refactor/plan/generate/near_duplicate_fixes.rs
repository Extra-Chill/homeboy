//! Auto-fix near-duplicate functions by removing the copy and re-exporting the canonical.
//!
//! Near-duplicate findings come in pairs: function `foo` in file A is structurally
//! identical to function `foo` in file B. This fixer:
//!
//! 1. Groups findings into pairs by function name
//! 2. Picks the canonical copy (first file alphabetically)
//! 3. Makes the canonical copy `pub(crate)` if it isn't already
//! 4. Removes the duplicate copy from the other file
//! 5. Adds a `use` import in the other file to reference the canonical
//!
//! Fixes are Safe — the audit has already verified structural identity.
//! The sandbox validation + cargo check gate catches any breakage before
//! the fix reaches main.

use std::collections::HashMap;
use std::path::Path;

use regex::Regex;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::refactor::auto::{Fix, FixSafetyTier, Insertion, InsertionKind, SkippedFile};

/// A parsed near-duplicate finding.
struct NearDupInfo {
    /// The function name that's duplicated.
    fn_name: String,
    /// The file containing this copy.
    file: String,
}

/// Generate fixes for near-duplicate functions.
pub(crate) fn generate_near_duplicate_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    // Pattern: "Near-duplicate `fn_name` — structurally identical to other/file.rs"
    let re = Regex::new(r"Near-duplicate `(\w+)` — structurally identical to (.+)")
        .expect("regex should compile");

    // Collect all near-duplicate findings.
    let mut infos: Vec<NearDupInfo> = Vec::new();
    for finding in &result.findings {
        if finding.kind != AuditFinding::NearDuplicate {
            continue;
        }

        let caps = match re.captures(&finding.description) {
            Some(c) => c,
            None => {
                skipped.push(SkippedFile {
                    file: finding.file.clone(),
                    reason: format!(
                        "Could not parse near-duplicate description: {}",
                        finding.description
                    ),
                });
                continue;
            }
        };

        infos.push(NearDupInfo {
            fn_name: caps[1].to_string(),
            file: finding.file.clone(),
        });
    }

    // Group by function name to find pairs.
    let mut groups: HashMap<String, Vec<NearDupInfo>> = HashMap::new();
    for info in infos {
        groups
            .entry(info.fn_name.clone())
            .or_default()
            .push(info);
    }

    for (fn_name, members) in &groups {
        if members.len() < 2 {
            // Lone finding without its pair — can't determine canonical.
            continue;
        }

        // Pick canonical: first file alphabetically.
        let mut files: Vec<&str> = members.iter().map(|m| m.file.as_str()).collect();
        files.sort();
        files.dedup();

        if files.len() < 2 {
            continue;
        }

        let canonical_file = files[0];
        let duplicate_file = files[1];

        // Read the duplicate file to find the function's line range.
        let dup_path = root.join(duplicate_file);
        let content = match std::fs::read_to_string(&dup_path) {
            Ok(c) => c,
            Err(_) => {
                skipped.push(SkippedFile {
                    file: duplicate_file.to_string(),
                    reason: format!("Could not read file: {}", duplicate_file),
                });
                continue;
            }
        };

        let Some((start_line, end_line)) = find_function_range(&content, fn_name) else {
            skipped.push(SkippedFile {
                file: duplicate_file.to_string(),
                reason: format!(
                    "Could not find function '{}' in {}",
                    fn_name, duplicate_file
                ),
            });
            continue;
        };

        // Build the module path for the import.
        // e.g., "src/core/code_audit/baseline.rs" → "crate::core::code_audit::baseline"
        let canonical_mod = file_to_module_path(canonical_file);

        // 1. Remove the duplicate function.
        let removal = Insertion {
            kind: InsertionKind::FunctionRemoval {
                start_line,
                end_line,
            },
            finding: AuditFinding::NearDuplicate,
            safety_tier: FixSafetyTier::Safe,
            auto_apply: false,
            blocked_reason: None,
            preflight: None,
            code: String::new(),
            description: format!(
                "Remove near-duplicate '{}' — canonical copy lives in {}",
                fn_name, canonical_file
            ),
        };

        // 2. Add import of the canonical function.
        let import_stmt = format!("use {}::{};", canonical_mod, fn_name);
        let import = Insertion {
            kind: InsertionKind::ImportAdd,
            finding: AuditFinding::NearDuplicate,
            safety_tier: FixSafetyTier::Safe,
            auto_apply: false,
            blocked_reason: None,
            preflight: None,
            code: import_stmt,
            description: format!(
                "Import '{}' from canonical location {}",
                fn_name, canonical_file
            ),
        };

        fixes.push(Fix {
            file: duplicate_file.to_string(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![removal, import],
            applied: false,
        });

        // 3. Ensure the canonical copy is pub(crate).
        let canon_path = root.join(canonical_file);
        if let Ok(canon_content) = std::fs::read_to_string(&canon_path) {
            if let Some(vis_fix) =
                build_visibility_upgrade(&canon_content, canonical_file, fn_name)
            {
                fixes.push(Fix {
                    file: canonical_file.to_string(),
                    required_methods: vec![],
                    required_registrations: vec![],
                    insertions: vec![vis_fix],
                    applied: false,
                });
            }
        }
    }
}

/// Find the line range (1-indexed, inclusive) of a function in Rust source code.
fn find_function_range(content: &str, fn_name: &str) -> Option<(usize, usize)> {
    let fn_re = Regex::new(&format!(
        r"(?:pub(?:\([^)]*\))?\s+)?fn\s+{}\s*[<(]",
        regex::escape(fn_name)
    ))
    .ok()?;

    let lines: Vec<&str> = content.lines().collect();

    let start_idx = lines.iter().position(|line| fn_re.is_match(line))?;

    // Walk forward counting braces to find the end.
    let mut depth = 0i32;
    let mut found_opening = false;
    for (i, line) in lines[start_idx..].iter().enumerate() {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    found_opening = true;
                }
                '}' => {
                    depth -= 1;
                    if found_opening && depth == 0 {
                        return Some((start_idx + 1, start_idx + i + 1)); // 1-indexed
                    }
                }
                _ => {}
            }
        }
    }

    None
}

/// Convert a file path like "src/core/code_audit/baseline.rs" to a Rust module path
/// like "crate::core::code_audit::baseline".
fn file_to_module_path(file: &str) -> String {
    let mut path = file.strip_prefix("src/").unwrap_or(file);
    // "foo/mod.rs" → "foo"
    if let Some(stripped) = path.strip_suffix("/mod.rs") {
        path = stripped;
    } else if let Some(stripped) = path.strip_suffix(".rs") {
        // "foo/bar.rs" → "foo/bar"
        path = stripped;
    }
    format!("crate::{}", path.replace('/', "::"))
}

/// If the canonical function is not already `pub` or `pub(crate)`, generate a
/// `VisibilityChange` insertion to make it `pub(crate)`.
fn build_visibility_upgrade(
    content: &str,
    file: &str,
    fn_name: &str,
) -> Option<Insertion> {
    let lines: Vec<&str> = content.lines().collect();

    // Find the function declaration line.
    let fn_re = Regex::new(&format!(r"fn\s+{}\s*[<(]", regex::escape(fn_name))).ok()?;
    let (line_idx, line_text) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| fn_re.is_match(line))?;

    let trimmed = line_text.trim();

    // Already pub or pub(crate) — no change needed.
    if trimmed.starts_with("pub fn")
        || trimmed.starts_with("pub(crate)")
        || trimmed.starts_with("pub(super)")
    {
        return None;
    }

    // It's a private `fn` — upgrade to `pub(crate) fn`.
    let _ = file; // used in description
    Some(Insertion {
        kind: InsertionKind::VisibilityChange {
            line: line_idx + 1,
            from: "fn ".to_string(),
            to: "pub(crate) fn ".to_string(),
        },
        finding: AuditFinding::NearDuplicate,
        safety_tier: FixSafetyTier::Safe,
        auto_apply: false,
        blocked_reason: None,
        preflight: None,
        code: String::new(),
        description: format!(
            "Make canonical '{}' pub(crate) so the duplicate's import resolves",
            fn_name
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_function_range_simple() {
        let content = "use foo;\n\nfn helper() {\n    println!(\"hi\");\n}\n\nfn main() {}\n";
        let range = find_function_range(content, "helper");
        assert_eq!(range, Some((3, 5)));
    }

    #[test]
    fn find_function_range_pub_crate() {
        let content = "pub(crate) fn load_baseline_from_ref(r: &str) -> Result<()> {\n    let x = 1;\n    Ok(())\n}\n";
        let range = find_function_range(content, "load_baseline_from_ref");
        assert_eq!(range, Some((1, 4)));
    }

    #[test]
    fn find_function_range_not_found() {
        let content = "fn other() {}\n";
        let range = find_function_range(content, "missing");
        assert_eq!(range, None);
    }

    #[test]
    fn file_to_module_path_standard() {
        assert_eq!(
            file_to_module_path("src/core/code_audit/baseline.rs"),
            "crate::core::code_audit::baseline"
        );
    }

    #[test]
    fn file_to_module_path_mod_rs() {
        assert_eq!(
            file_to_module_path("src/core/code_audit/mod.rs"),
            "crate::core::code_audit"
        );
    }

    #[test]
    fn build_visibility_upgrade_private_fn() {
        let content = "fn cache_path() -> PathBuf {\n    dirs::cache_dir().unwrap()\n}\n";
        let ins = build_visibility_upgrade(content, "test.rs", "cache_path");
        assert!(ins.is_some());
        let ins = ins.unwrap();
        assert!(matches!(
            ins.kind,
            InsertionKind::VisibilityChange { line: 1, .. }
        ));
        assert_eq!(ins.safety_tier, FixSafetyTier::Safe);
    }

    #[test]
    fn build_visibility_upgrade_already_pub() {
        let content = "pub fn cache_path() -> PathBuf {\n    dirs::cache_dir().unwrap()\n}\n";
        let ins = build_visibility_upgrade(content, "test.rs", "cache_path");
        assert!(ins.is_none(), "Should not upgrade already-pub function");
    }

    #[test]
    fn build_visibility_upgrade_already_pub_crate() {
        let content =
            "pub(crate) fn cache_path() -> PathBuf {\n    dirs::cache_dir().unwrap()\n}\n";
        let ins = build_visibility_upgrade(content, "test.rs", "cache_path");
        assert!(
            ins.is_none(),
            "Should not upgrade already-pub(crate) function"
        );
    }
}
