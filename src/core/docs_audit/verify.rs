//! Claim verification against the codebase.
//!
//! Verifies claims extracted from documentation against the actual codebase.
//! Some claims can be verified mechanically (file exists), others require
//! manual verification by an agent.

use std::path::Path;

use super::claims::{Claim, ClaimType};

/// Result of verifying a claim.
#[derive(Debug, Clone)]
pub enum VerifyResult {
    /// Claim verified as true
    Verified,
    /// Claim verified as false
    Broken {
        suggestion: Option<String>,
    },
    /// Cannot verify mechanically - agent must check
    NeedsVerification {
        hint: String,
    },
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

    // Try multiple path interpretations
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

    if Path::new(path).is_absolute() {
        return VerifyResult::NeedsVerification {
            hint: "Absolute path outside repository; verify path exists on target system."
                .to_string(),
        };
    }

    VerifyResult::Broken {
        suggestion: Some(format!(
            "File '{}' not found. Search codebase for actual location or remove if deleted.",
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

    // Try multiple path interpretations
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

    if Path::new(path).is_absolute() {
        return VerifyResult::NeedsVerification {
            hint: "Absolute directory path outside repository; verify path exists on target system."
                .to_string(),
        };
    }

    VerifyResult::Broken {
        suggestion: Some(format!(
            "Directory '{}' not found. Check if directory was moved or renamed.",
            path
        )),
    }
}

/// Verify a code example claim.
fn verify_code_example(_claim: &Claim) -> VerifyResult {
    // Code examples always require manual verification
    // We can't know if the syntax is still valid without understanding the API
    VerifyResult::NeedsVerification {
        hint: "Verify code example syntax matches current API. Check that all referenced functions, classes, and parameters are correct.".to_string(),
    }
}

#[cfg(test)]
mod tests {
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
            value: "/var/lib/sweatpants/modules.yaml".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
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
            value: "/var/lib/sweatpants/".to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
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
            context: None,
        };

        // Should verify by stripping the component prefix
        let result = verify_file_path(&claim, temp_dir.path(), temp_dir.path(), Some("homeboy"));
        assert!(matches!(result, VerifyResult::Verified));
    }
}
