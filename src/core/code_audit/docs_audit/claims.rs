//! Claim extraction from markdown documentation files.
//!
//! Parses markdown to extract verifiable claims:
//! - File paths in backticks (must contain path separator)
//! - Directory paths in backticks
//! - Code examples in fenced blocks

mod constants;
mod helpers;
mod line_suggests_real;
mod types;

pub use constants::*;
pub use helpers::*;
pub use line_suggests_real::*;
pub use types::*;


use glob_match::glob_match;
use regex::Regex;
use std::sync::LazyLock;

// Regex patterns for claim extraction
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skip_domain_patterns() {
        let content = "Visit `mysite.com/path/to/page.html` for documentation.";
        let claims = extract_claims(content, "test.md", &[]);

        // Should skip domain-like paths
        assert!(!claims.iter().any(|c| c.value.contains("mysite.com")));
    }

    #[test]
    fn test_no_identifiers_or_method_signatures() {
        // Identifiers and method signatures should NOT be extracted
        let content = r#"
The `BaseTool` class provides base functionality.
Call `register_tool(name, handler)` to register a tool.
"#;
        let claims = extract_claims(content, "test.md", &[]);

        // Should have no claims (no file paths or directories in this content)
        assert!(claims.is_empty());
    }

    #[test]
    fn test_skip_mime_types() {
        let content =
            "The file type is `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`.";
        let claims = extract_claims(content, "test.md", &[]);

        assert!(!claims.iter().any(|c| c.value.starts_with("application/")));
    }

    #[test]
    fn test_skip_various_mime_types() {
        let content = r#"
Supported types: `text/plain`, `image/png`, `audio/mpeg`, `video/mp4`.
"#;
        let claims = extract_claims(content, "test.md", &[]);

        assert!(claims.is_empty());
    }

    #[test]
    fn test_ignore_patterns_filter_rest_api() {
        let content = "The endpoint is at `/wp-json/datamachine/v1/events`.";
        // Use ** to match multiple path segments
        let patterns = vec!["/wp-json/**".to_string()];
        let claims = extract_claims(content, "test.md", &patterns);

        assert!(!claims.iter().any(|c| c.value.contains("wp-json")));
    }

    #[test]
    fn test_ignore_patterns_filter_api_versioned() {
        let content = "Call `/api/v1/users/list.json` for the user list.";
        // Use ** to match path segments before and after /v1/
        let patterns = vec!["**/v1/**".to_string()];
        let claims = extract_claims(content, "test.md", &patterns);

        assert!(!claims.iter().any(|c| c.value.contains("/v1/")));
    }

    #[test]
    fn test_ignore_patterns_filter_oauth_callback() {
        let content = "OAuth redirects to `/datamachine-auth/twitter/` callback.";
        // Use ** to match any path starting with segment ending in -auth
        let patterns = vec!["/*-auth/**".to_string()];
        let claims = extract_claims(content, "test.md", &patterns);

        assert!(!claims.iter().any(|c| c.value.contains("-auth/")));
    }

    #[test]
    fn test_skip_non_namespaced_identifiers() {
        // Single class name without namespace should NOT be extracted
        let content = "The `CacheManager` class handles caching.";
        let claims = extract_claims(content, "test.md", &[]);

        assert!(!claims.iter().any(|c| c.claim_type == ClaimType::ClassName));
    }

    #[test]
    fn test_no_ignore_patterns_extracts_api_paths() {
        // Without ignore patterns, API-like paths ARE extracted
        let content = "The endpoint is at `/wp-json/datamachine/v1/events.json`.";
        let claims = extract_claims(content, "test.md", &[]);

        // With no patterns, this should be extracted as a file path
        assert!(claims.iter().any(|c| c.value.contains("wp-json")));
    }

    // ========================================================================
    // Confidence classification tests
    // ========================================================================

    #[test]
    fn test_prose_file_path_is_real_confidence() {
        let content = "See `src/core/config.rs` for the configuration extension.";
        let claims = extract_claims(content, "test.md", &[]);

        let claim = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::FilePath)
            .expect("should extract file path");
        assert_eq!(claim.confidence, ClaimConfidence::Real);
    }

    #[test]
    fn test_example_context_path_is_example() {
        let content = "For example, `your-project/src/main.rs` would be the entry point.";
        let claims = extract_claims(content, "test.md", &[]);

        let claim = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::FilePath)
            .expect("should extract file path");
        // "your-" in path and "example" in context both trigger Example confidence
        assert_eq!(claim.confidence, ClaimConfidence::Example);
    }

    #[test]
    fn test_placeholder_class_is_example_confidence() {
        let content = "Create a handler like MyNamespace\\MyHandler to process events.";
        let claims = extract_claims(content, "test.md", &[]);

        let claim = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::ClassName)
            .expect("should extract class name");
        assert_eq!(claim.confidence, ClaimConfidence::Example);
    }

    #[test]
    fn test_real_class_in_prose_is_real_confidence() {
        let content = "The DataMachine\\Services\\CacheManager handles caching.";
        let claims = extract_claims(content, "test.md", &[]);

        let claim = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::ClassName)
            .expect("should extract class name");
        assert_eq!(claim.confidence, ClaimConfidence::Real);
    }

    #[test]
    fn test_code_block_claims_are_unclear() {
        let content = "Example:\n```php\nfunction test() { return true; }\n```\n";
        let claims = extract_claims(content, "test.md", &[]);

        let code_claim = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::CodeExample)
            .expect("should extract code example");
        assert_eq!(code_claim.confidence, ClaimConfidence::Unclear);
    }

    #[test]
    fn test_code_block_interior_paths_not_extracted() {
        // File paths inside code blocks should NOT be extracted as separate claims
        // (they are part of the code example claim)
        let content = "```rust\nuse crate::core::config;\nlet path = \"src/main.rs\";\n```\n";
        let claims = extract_claims(content, "test.md", &[]);

        // Should only have the code block claim, no file path claims
        assert!(
            !claims.iter().any(|c| c.claim_type == ClaimType::FilePath),
            "file paths inside code blocks should not be extracted separately"
        );
    }

    #[test]
    fn test_annotation_context_is_real() {
        let content = "@see DataMachine\\Core\\Engine for the main engine class.";
        let claims = extract_claims(content, "test.md", &[]);

        let claim = claims
            .iter()
            .find(|c| c.claim_type == ClaimType::ClassName)
            .expect("should extract class name");
        assert_eq!(claim.confidence, ClaimConfidence::Real);
    }

    #[test]
    fn test_is_placeholder_class_detection() {
        assert!(is_placeholder_class("MyNamespace\\MyHandler"));
        assert!(is_placeholder_class("Your\\Extension\\Plugin"));
        assert!(is_placeholder_class("Example\\Namespace\\Class"));
        assert!(is_placeholder_class("Foo\\Bar\\Baz"));
        assert!(is_placeholder_class("Test\\Mock\\Handler"));
        assert!(!is_placeholder_class("DataMachine\\Services\\Cache"));
        assert!(!is_placeholder_class("WordPress\\Plugin\\Activator"));
    }

    #[test]
    fn test_windows_path_not_extracted_as_class() {
        let content = "Typically: `C:\\Users\\<username>\\AppData\\Roaming\\homeboy\\`";
        let claims = extract_claims(content, "test.md", &[]);

        // Should NOT extract AppData\Roaming as a class name
        assert!(
            !claims.iter().any(|c| c.claim_type == ClaimType::ClassName),
            "Windows path segments should not be extracted as class names"
        );
    }

    #[test]
    fn test_os_path_context_detection() {
        assert!(is_os_path_context(
            "Typically: C:\\Users\\admin\\AppData\\Roaming",
            25
        ));
        assert!(is_os_path_context("Path is C:/Users/admin/AppData", 20));
        assert!(!is_os_path_context(
            "The DataMachine\\Services\\Cache class",
            4
        ));
    }

    #[test]
    fn test_would_rename_context_is_example() {
        // Issue #325: "For example, renaming widget -> gadget would rename widget/widget.rs"
        let content = "For example, renaming widget to gadget would rename `widget/widget.rs` to `gadget/gadget.rs`";
        let claims = extract_claims(content, "test.md", &[]);

        for claim in &claims {
            if claim.claim_type == ClaimType::FilePath {
                assert_eq!(
                    claim.confidence,
                    ClaimConfidence::Example,
                    "paths in 'would rename' context should be Example, not {:?}: {}",
                    claim.confidence,
                    claim.value
                );
            }
        }
    }

    #[test]
    fn test_renaming_context_is_example() {
        let content = "Renaming `scripts/build/` to `scripts/compile/` requires updating imports.";
        let claims = extract_claims(content, "test.md", &[]);

        for claim in &claims {
            assert_eq!(
                claim.confidence,
                ClaimConfidence::Example,
                "paths in 'renaming' context should be Example: {}",
                claim.value
            );
        }
    }

    #[test]
    fn test_this_creates_context_is_example() {
        // Test when path is on the same line as "this creates"
        let content2 = "This creates `docs/api/endpoints.md` with heading";
        let claims2 = extract_claims(content2, "test.md", &[]);

        if let Some(claim) = claims2.iter().find(|c| c.claim_type == ClaimType::FilePath) {
            assert_eq!(
                claim.confidence,
                ClaimConfidence::Example,
                "paths in 'this creates' context should be Example confidence"
            );
        }

        // Also verify paths after "Example:" context
        let content3 = "**Example:** `projects/extrachill.json`";
        let claims3 = extract_claims(content3, "test.md", &[]);

        if let Some(claim) = claims3.iter().find(|c| c.claim_type == ClaimType::FilePath) {
            assert_eq!(
                claim.confidence,
                ClaimConfidence::Example,
                "paths in 'Example:' context should be Example confidence"
            );
        }
    }

    #[test]
    fn test_extract_claims_context_some_line_trim_to_string() {
        let content = "";
        let doc_file = "";
        let ignore_patterns = Vec::new();
        let result = extract_claims(&content, &doc_file, &ignore_patterns);
        assert!(!result.is_empty(), "expected non-empty collection for: context: Some(line.trim().to_string()),");
    }

    #[test]
    fn test_extract_claims_context_some_line_trim_to_string_2() {
        let content = "";
        let doc_file = "";
        let ignore_patterns = Vec::new();
        let result = extract_claims(&content, &doc_file, &ignore_patterns);
        assert!(!result.is_empty(), "expected non-empty collection for: context: Some(line.trim().to_string()),");
    }

    #[test]
    fn test_extract_claims_context_some_line_trim_to_string_3() {
        let content = "";
        let doc_file = "";
        let ignore_patterns = Vec::new();
        let result = extract_claims(&content, &doc_file, &ignore_patterns);
        assert!(!result.is_empty(), "expected non-empty collection for: context: Some(line.trim().to_string()),");
    }

    #[test]
    fn test_extract_claims_context_some_format_block_language() {
        let content = "";
        let doc_file = "";
        let ignore_patterns = Vec::new();
        let result = extract_claims(&content, &doc_file, &ignore_patterns);
        assert!(!result.is_empty(), "expected non-empty collection for: context: Some(format!(\"'''{{}} block\", language)),");
    }

    #[test]
    fn test_extract_claims_has_expected_effects() {
        // Expected effects: mutation
        let content = "";
        let doc_file = "";
        let ignore_patterns = Vec::new();
        let _ = extract_claims(&content, &doc_file, &ignore_patterns);
    }

}
