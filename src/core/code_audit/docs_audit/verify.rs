//! Claim verification against the codebase.
//!
//! Verifies claims extracted from documentation against the actual codebase.
//! Some claims can be verified mechanically (file exists), others require
//! manual verification by an agent.

use std::fs;
use std::path::Path;

use super::claims::{Claim, ClaimType};

/// Result of verifying a claim.
#[derive(Debug, Clone)]
pub enum VerifyResult {
    /// Claim verified as true
    Verified,
    /// Claim verified as false
    Broken { suggestion: Option<String> },
    /// Cannot verify mechanically - agent must check
    NeedsVerification { hint: String },
}

/// Verify a claim against the codebase.
///
/// The `component_id` is used to strip component-prefixed paths (e.g., `homeboy/docs/index.md`
/// becomes `docs/index.md` when verifying against the homeboy component).
pub fn verify_claim(
    claim: &Claim,
    source_path: &Path,
    docs_path: &Path,
    component_id: Option<&str>,
) -> VerifyResult {
    match claim.claim_type {
        ClaimType::FilePath => verify_file_path(claim, source_path, docs_path, component_id),
        ClaimType::DirectoryPath => {
            verify_directory_path(claim, source_path, docs_path, component_id)
        }
        ClaimType::CodeExample => verify_code_example(claim),
        ClaimType::ClassName => verify_class_name(claim, source_path),
    }
}

/// Strip component prefix from a path if present.
///
/// Example: `homeboy/docs/index.md` with component_id `homeboy` returns `docs/index.md`
fn strip_component_prefix<'a>(path: &'a str, component_id: Option<&str>) -> &'a str {
    if let Some(id) = component_id {
        let prefix = format!("{}/", id);
        if path.starts_with(&prefix) {
            return &path[prefix.len()..];
        }
    }
    path
}

/// Verify a file path claim.
fn verify_file_path(
    claim: &Claim,
    source_path: &Path,
    docs_path: &Path,
    component_id: Option<&str>,
) -> VerifyResult {
    let path = &claim.value;

    // Strip component prefix if present (e.g., "homeboy/docs/index.md" -> "docs/index.md")
    let stripped_path = strip_component_prefix(path, component_id);

    // Absolute paths can't be verified against the source tree — they reference
    // system paths that may or may not exist on the current machine. Return early
    // to avoid Path::join replacing the base with the absolute path and accidentally
    // checking the real filesystem.
    if Path::new(path).is_absolute() {
        return VerifyResult::NeedsVerification {
            hint: "Absolute path outside repository; verify path exists on target system."
                .to_string(),
        };
    }

    // Try multiple path interpretations for relative paths
    let candidates = vec![
        source_path.join(stripped_path.trim_start_matches('/')),
        source_path.join(stripped_path),
        docs_path.join(stripped_path.trim_start_matches('/')),
        docs_path.join(stripped_path),
        // Also try original path in case stripping was wrong
        source_path.join(path.trim_start_matches('/')),
        source_path.join(path),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return VerifyResult::Verified;
        }
    }

    // File not found — try to find a similar file (same name, different location)
    if let Some(similar) = find_similar_file(source_path, stripped_path) {
        return VerifyResult::Broken {
            suggestion: Some(format!(
                "Did you mean `{}`? File '{}' no longer exists at the documented path.",
                similar, path
            )),
        };
    }

    VerifyResult::Broken {
        suggestion: Some(format!(
            "File '{}' no longer exists. Update or remove this reference from documentation.",
            path
        )),
    }
}

/// Verify a directory path claim.
fn verify_directory_path(
    claim: &Claim,
    source_path: &Path,
    docs_path: &Path,
    component_id: Option<&str>,
) -> VerifyResult {
    let path = &claim.value;

    // Strip component prefix if present
    let stripped_path = strip_component_prefix(path, component_id);

    // Absolute paths can't be verified against the source tree — return early
    // to avoid Path::join replacing the base with the absolute path.
    if Path::new(path).is_absolute() {
        return VerifyResult::NeedsVerification {
            hint:
                "Absolute directory path outside repository; verify path exists on target system."
                    .to_string(),
        };
    }

    // Try multiple path interpretations for relative paths
    let candidates = vec![
        source_path.join(stripped_path.trim_start_matches('/')),
        source_path.join(stripped_path),
        docs_path.join(stripped_path.trim_start_matches('/')),
        docs_path.join(stripped_path),
        // Also try original path in case stripping was wrong
        source_path.join(path.trim_start_matches('/')),
        source_path.join(path),
    ];

    for candidate in &candidates {
        if candidate.is_dir() {
            return VerifyResult::Verified;
        }
    }

    // Directory not found — try to find a similar directory
    if let Some(similar) = find_similar_dir(source_path, stripped_path) {
        return VerifyResult::Broken {
            suggestion: Some(format!(
                "Did you mean `{}`? Directory '{}' no longer exists at the documented path.",
                similar, path
            )),
        };
    }

    VerifyResult::Broken {
        suggestion: Some(format!(
            "Directory '{}' no longer exists. Update or remove this reference from documentation.",
            path
        )),
    }
}

/// Verify a namespaced class reference by searching for the class definition in source files.
///
/// Converts namespace path to directory structure (e.g., `DataMachine\Services\CacheManager`
/// becomes a search for `class CacheManager` in files under a path matching the namespace).
fn verify_class_name(claim: &Claim, source_path: &Path) -> VerifyResult {
    let class_ref = &claim.value;

    // Split into segments: DataMachine\Services\CacheManager -> ["DataMachine", "Services", "CacheManager"]
    let segments: Vec<&str> = class_ref.split('\\').collect();
    if segments.len() < 2 {
        return VerifyResult::NeedsVerification {
            hint: "Class reference too short to verify.".to_string(),
        };
    }

    let class_name = segments.last().unwrap();

    // Search for the class definition in source files
    if search_class_in_dir(source_path, class_name) {
        return VerifyResult::Verified;
    }

    VerifyResult::Broken {
        suggestion: Some(format!(
            "Class '{}' not found in source. Update documentation to reflect current class name, or remove if deleted.",
            class_ref
        )),
    }
}

/// Recursively search for a class/struct/trait definition in source files.
fn search_class_in_dir(dir: &Path, class_name: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden dirs, vendor, node_modules, target, etc.
        if name.starts_with('.')
            || name == "vendor"
            || name == "node_modules"
            || name == "target"
            || name == "__pycache__"
        {
            continue;
        }

        if path.is_dir() {
            if search_class_in_dir(&path, class_name) {
                return true;
            }
        } else if path.is_file() {
            // Only check source files
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(
                ext,
                "php" | "rs" | "py" | "js" | "ts" | "go" | "java" | "rb" | "kt" | "swift"
            ) {
                continue;
            }

            if let Ok(content) = fs::read_to_string(&path) {
                // Check for class/struct/trait/interface definitions
                // PHP: class CacheManager, interface CacheManager, trait CacheManager
                // Rust: struct CacheManager, enum CacheManager, trait CacheManager
                // Python: class CacheManager
                for line in content.lines() {
                    let trimmed = line.trim();
                    if (trimmed.contains(&format!("class {}", class_name))
                        || trimmed.contains(&format!("struct {}", class_name))
                        || trimmed.contains(&format!("trait {}", class_name))
                        || trimmed.contains(&format!("interface {}", class_name))
                        || trimmed.contains(&format!("enum {}", class_name)))
                        && !trimmed.starts_with("//")
                        && !trimmed.starts_with('#')
                        && !trimmed.starts_with('*')
                    {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Verify a code example claim.
fn verify_code_example(_claim: &Claim) -> VerifyResult {
    // Code examples always require manual verification
    // We can't know if the syntax is still valid without understanding the API
    VerifyResult::NeedsVerification {
        hint: "Code example may be stale. Compare against current implementation and update documentation if it no longer matches.".to_string(),
    }
}

// ============================================================================
// Fuzzy path matching — find files/dirs that moved
// ============================================================================

/// Search the source tree for a file with the same name as the missing reference.
///
/// If `src/old_dir/config.rs` is missing but `src/new_dir/config.rs` exists,
/// returns `Some("src/new_dir/config.rs")`.
fn find_similar_file(root: &Path, missing_path: &str) -> Option<String> {
    let target_name = Path::new(missing_path).file_name()?.to_str()?;

    // Don't fuzzy-match very generic filenames
    if matches!(
        target_name,
        "mod.rs" | "lib.rs" | "main.rs" | "index.js" | "index.ts" | "index.php" | "__init__.py"
    ) {
        return None;
    }

    let mut matches = Vec::new();
    collect_files_named(root, root, target_name, &mut matches);

    if matches.len() == 1 {
        return Some(matches.into_iter().next().unwrap());
    }

    if matches.len() > 1 {
        // Multiple matches — pick the one most similar to the original path
        let missing_parts: Vec<&str> = missing_path.split('/').collect();
        matches.sort_by_key(|m| {
            let parts: Vec<&str> = m.split('/').collect();
            let shared = missing_parts.iter().filter(|p| parts.contains(p)).count();
            -(shared as i32)
        });
        return Some(matches.into_iter().next().unwrap());
    }

    None
}

/// Search the source tree for a directory with the same name.
fn find_similar_dir(root: &Path, missing_path: &str) -> Option<String> {
    let clean = missing_path.trim_end_matches('/');
    let target_name = Path::new(clean).file_name()?.to_str()?;

    let mut matches = Vec::new();
    collect_dirs_named(root, root, target_name, &mut matches);

    if matches.len() == 1 {
        return Some(format!("{}/", matches.into_iter().next().unwrap()));
    }

    if matches.len() > 1 {
        let missing_parts: Vec<&str> = clean.split('/').collect();
        matches.sort_by_key(|m| {
            let parts: Vec<&str> = m.split('/').collect();
            let shared = missing_parts.iter().filter(|p| parts.contains(p)).count();
            -(shared as i32)
        });
        return Some(format!("{}/", matches.into_iter().next().unwrap()));
    }

    None
}

/// Recursively collect files matching a target filename.
fn collect_files_named(root: &Path, dir: &Path, target: &str, results: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if name.starts_with('.')
            || matches!(
                name.as_str(),
                "node_modules" | "vendor" | "target" | "__pycache__" | ".git"
            )
        {
            continue;
        }

        if path.is_dir() {
            collect_files_named(root, &path, target, results);
        } else if path.is_file() && name == target {
            if let Ok(rel) = path.strip_prefix(root) {
                results.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

/// Recursively collect directories matching a target name.
fn collect_dirs_named(root: &Path, dir: &Path, target: &str, results: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if name.starts_with('.')
            || matches!(
                name.as_str(),
                "node_modules" | "vendor" | "target" | "__pycache__" | ".git"
            )
        {
            continue;
        }

        if path.is_dir() {
            if name == target {
                if let Ok(rel) = path.strip_prefix(root) {
                    results.push(rel.to_string_lossy().to_string());
                }
            }
            collect_dirs_named(root, &path, target, results);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::claims::ClaimConfidence;
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_verify_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let claim = Claim {
            claim_type: ClaimType::FilePath,
            value: "test.rs".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let result = verify_file_path(&claim, temp_dir.path(), temp_dir.path(), None);
        assert!(matches!(result, VerifyResult::Verified));
    }

    #[test]
    fn test_verify_missing_file() {
        let temp_dir = TempDir::new().unwrap();

        let claim = Claim {
            claim_type: ClaimType::FilePath,
            value: "nonexistent.rs".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let result = verify_file_path(&claim, temp_dir.path(), temp_dir.path(), None);
        assert!(matches!(result, VerifyResult::Broken { .. }));
    }

    #[test]
    fn test_verify_absolute_path_needs_verification() {
        let temp_dir = TempDir::new().unwrap();

        let claim = Claim {
            claim_type: ClaimType::FilePath,
            value: "/var/lib/sweatpants/extensions.yaml".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let result = verify_file_path(&claim, temp_dir.path(), temp_dir.path(), None);
        assert!(matches!(result, VerifyResult::NeedsVerification { .. }));
    }

    #[test]
    fn test_verify_existing_directory() {
        let temp_dir = TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("src/core");
        fs::create_dir_all(&dir_path).unwrap();

        let claim = Claim {
            claim_type: ClaimType::DirectoryPath,
            value: "src/core/".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let result = verify_directory_path(&claim, temp_dir.path(), temp_dir.path(), None);
        assert!(matches!(result, VerifyResult::Verified));
    }

    #[test]
    fn test_verify_absolute_directory_needs_verification() {
        let temp_dir = TempDir::new().unwrap();

        let claim = Claim {
            claim_type: ClaimType::DirectoryPath,
            value: "/opt/nonexistent-test-path-xyz/".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let result = verify_directory_path(&claim, temp_dir.path(), temp_dir.path(), None);
        assert!(matches!(result, VerifyResult::NeedsVerification { .. }));
    }

    #[test]
    fn test_strip_component_prefix() {
        // With component ID
        assert_eq!(
            strip_component_prefix("homeboy/docs/index.md", Some("homeboy")),
            "docs/index.md"
        );

        // Without matching prefix
        assert_eq!(
            strip_component_prefix("other/docs/index.md", Some("homeboy")),
            "other/docs/index.md"
        );

        // Without component ID
        assert_eq!(
            strip_component_prefix("homeboy/docs/index.md", None),
            "homeboy/docs/index.md"
        );
    }

    #[test]
    fn test_verify_file_with_component_prefix() {
        let temp_dir = TempDir::new().unwrap();

        // Create docs/index.md (without component prefix)
        let docs_dir = temp_dir.path().join("docs");
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(docs_dir.join("index.md"), "# Docs").unwrap();

        // Claim references "homeboy/docs/index.md" (with component prefix)
        let claim = Claim {
            claim_type: ClaimType::FilePath,
            value: "homeboy/docs/index.md".to_string(),
            doc_file: "test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        // Should verify by stripping the component prefix
        let result = verify_file_path(&claim, temp_dir.path(), temp_dir.path(), Some("homeboy"));
        assert!(matches!(result, VerifyResult::Verified));
    }
}
