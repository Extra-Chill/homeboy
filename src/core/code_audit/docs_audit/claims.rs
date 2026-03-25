//! Claim extraction from markdown documentation files.
//!
//! Parses markdown to extract verifiable claims:
//! - File paths in backticks (must contain path separator)
//! - Directory paths in backticks
//! - Code examples in fenced blocks

mod constants;
mod helpers;
mod types;

pub use constants::*;
pub use helpers::*;
pub use types::*;


use glob_match::glob_match;
use regex::Regex;
use std::sync::LazyLock;

// Regex patterns for claim extraction
/// Check if a line's context suggests a real reference (annotation, cross-ref).
fn line_suggests_real(line: &str) -> bool {
    line.contains("@see")
        || line.contains("@uses")
        || line.contains("@link")
        || line.contains("@param")
        || line.contains("@return")
        || line.contains("@throws")
}

/// Classify confidence for a file/directory path claim.
fn classify_path_confidence(value: &str, line: &str, in_code_block: bool) -> ClaimConfidence {
    if in_code_block {
        return ClaimConfidence::Example;
    }
    if line_suggests_real(line) {
        return ClaimConfidence::Real;
    }
    if line_suggests_example(line) {
        return ClaimConfidence::Example;
    }
    // Path references in prose default to real — they should resolve
    let lower = value.to_lowercase();
    if lower.contains("example") || lower.contains("sample") || lower.contains("your-") {
        return ClaimConfidence::Example;
    }
    ClaimConfidence::Real
}

/// Classify confidence for a class name claim.
fn classify_class_confidence(value: &str, line: &str, in_code_block: bool) -> ClaimConfidence {
    if is_placeholder_class(value) {
        return ClaimConfidence::Example;
    }
    if in_code_block {
        // Inside a code block with a non-placeholder name — could be real or example
        if line_suggests_real(line) {
            return ClaimConfidence::Real;
        }
        return ClaimConfidence::Unclear;
    }
    if line_suggests_example(line) {
        return ClaimConfidence::Example;
    }
    ClaimConfidence::Real
}

/// Extract all claims from a markdown document.
///
/// The `ignore_patterns` parameter allows components to filter out platform-specific
/// patterns (e.g., `/wp-json/*` for WordPress) without hardcoding them in core.
pub fn extract_claims(content: &str, doc_file: &str, ignore_patterns: &[String]) -> Vec<Claim> {
    let mut claims = Vec::new();

    // Track which positions we've already claimed to avoid duplicates
    let mut claimed_positions: Vec<(usize, usize)> = Vec::new();

    // Track whether we're inside a fenced code block
    let mut in_code_block = false;

    // Process line by line for line numbers
    for (line_idx, line) in content.lines().enumerate() {
        let line_num = line_idx + 1;

        // Toggle code block state on fence lines
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        // Skip inline extraction for lines inside code blocks —
        // code blocks are handled separately as CodeExample claims
        if in_code_block {
            continue;
        }

        // Extract file paths
        for cap in FILE_PATH_PATTERN.captures_iter(line) {
            let full_match = cap.get(0).unwrap();
            let pos = (line_idx, full_match.start());

            if !claimed_positions.contains(&pos) {
                let path = cap.get(1).map(|m| m.as_str()).unwrap_or("");

                // Skip if it looks like a URL
                if path.contains("://") || path.starts_with("http") {
                    continue;
                }

                // Skip domain-like patterns (mysite.com, example.org)
                if is_domain_like(path) {
                    continue;
                }

                // Skip MIME types (application/*, text/*, etc.)
                if is_mime_type(path) {
                    continue;
                }

                // Skip component-configured ignore patterns
                if matches_ignore_pattern(path, ignore_patterns) {
                    continue;
                }

                // Skip very short paths that might be false positives
                if path.len() < 5 {
                    continue;
                }

                let confidence = classify_path_confidence(path, line, false);

                claimed_positions.push(pos);
                claims.push(Claim {
                    claim_type: ClaimType::FilePath,
                    value: path.to_string(),
                    doc_file: doc_file.to_string(),
                    line: line_num,
                    confidence,
                    context: Some(line.trim().to_string()),
                });
            }
        }

        // Extract namespaced class references
        for cap in CLASS_NAME_PATTERN.captures_iter(line) {
            let full_match = cap.get(0).unwrap();
            let pos = (line_idx, full_match.start());

            if !claimed_positions.contains(&pos) {
                let class_ref = cap.get(1).map(|m| m.as_str()).unwrap_or("");

                // Skip if this looks like part of a Windows/OS filesystem path
                // (e.g., C:\Users\<username>\AppData\Roaming)
                if is_os_path_context(line, full_match.start()) {
                    continue;
                }

                // Normalize double backslashes to single
                let normalized = class_ref.replace("\\\\", "\\");

                // Skip component-configured ignore patterns
                if matches_ignore_pattern(&normalized, ignore_patterns) {
                    continue;
                }

                let confidence = classify_class_confidence(&normalized, line, false);

                claimed_positions.push(pos);
                claims.push(Claim {
                    claim_type: ClaimType::ClassName,
                    value: normalized,
                    doc_file: doc_file.to_string(),
                    line: line_num,
                    confidence,
                    context: Some(line.trim().to_string()),
                });
            }
        }

        // Extract directory paths
        for cap in DIR_PATH_PATTERN.captures_iter(line) {
            let full_match = cap.get(0).unwrap();
            let pos = (line_idx, full_match.start());

            if !claimed_positions.contains(&pos) {
                let path = cap.get(1).map(|m| m.as_str()).unwrap_or("");

                // Skip common false positives
                if path == "./" || path == "../" || path.len() < 4 {
                    continue;
                }

                // Skip component-configured ignore patterns
                if matches_ignore_pattern(path, ignore_patterns) {
                    continue;
                }

                let confidence = classify_path_confidence(path, line, false);

                claimed_positions.push(pos);
                claims.push(Claim {
                    claim_type: ClaimType::DirectoryPath,
                    value: path.to_string(),
                    doc_file: doc_file.to_string(),
                    line: line_num,
                    confidence,
                    context: Some(line.trim().to_string()),
                });
            }
        }
    }

    // Extract code blocks
    for cap in CODE_BLOCK_PATTERN.captures_iter(content) {
        let language = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let code = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        // Find the line number of this code block
        let block_start = cap.get(0).unwrap().start();
        let line_num = content[..block_start].lines().count() + 1;

        // Only track code blocks for languages we care about
        if matches!(
            language,
            "php" | "rust" | "js" | "javascript" | "ts" | "typescript" | "python" | "go"
        ) {
            claims.push(Claim {
                claim_type: ClaimType::CodeExample,
                value: code.trim().to_string(),
                doc_file: doc_file.to_string(),
                line: line_num,
                // Code examples are inherently unclear — they may be illustrative
                confidence: ClaimConfidence::Unclear,
                context: Some(format!("```{} block", language)),
            });
        }
    }

    claims
}

/// Check if a path looks like a domain name rather than a file path.
///
/// Checks if any segment of the path contains a domain extension (e.g., `mysite.com/path`).
fn is_domain_like(path: &str) -> bool {
    let lower = path.to_lowercase();
    // Check if any part of the path contains a domain extension
    // This catches both "mysite.com" and "mysite.com/path/to/file.html"
    DOMAIN_EXTENSIONS
        .iter()
        .any(|ext| lower.contains(&format!("{ext}/")) || lower.ends_with(ext))
}

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
