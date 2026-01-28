//! Claim extraction from markdown documentation files.
//!
//! Parses markdown to extract verifiable claims:
//! - File paths in backticks (must contain path separator)
//! - Directory paths in backticks
//! - Code examples in fenced blocks

use glob_match::glob_match;
use regex::Regex;
use std::sync::LazyLock;

/// Types of claims that can be extracted from documentation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    /// File path reference (e.g., `src/main.rs`, `/inc/foo/bar.php`)
    FilePath,
    /// Directory path reference (e.g., `src/core/`, `/inc/Engine/`)
    DirectoryPath,
    /// Code example in a fenced block
    CodeExample,
}

/// A claim extracted from documentation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Claim {
    pub claim_type: ClaimType,
    pub value: String,
    pub doc_file: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

// Regex patterns for claim extraction
static FILE_PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches paths that contain at least one directory separator
    // e.g., `src/main.rs`, `/inc/Engine/AI/Tools/BaseTool.php`, `path/to/file.ext`
    // Must have: path separator + file extension
    Regex::new(r"`(/?(?:[\w.-]+/)+[\w.-]+\.[a-zA-Z0-9]+)`").unwrap()
});

static DIR_PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches directory paths like `/inc/Engine/` or `src/core/` in backticks
    // Must end with / and contain at least one directory level
    Regex::new(r"`(/?(?:[\w.-]+/)+)`").unwrap()
});

static CODE_BLOCK_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches fenced code blocks with language identifier
    Regex::new(r"(?s)```(\w+)\n(.*?)```").unwrap()
});

/// Extensions that indicate domain-like patterns (not file paths)
const DOMAIN_EXTENSIONS: &[&str] = &[
    ".com", ".org", ".io", ".net", ".dev", ".co", ".app", ".ai", ".xyz",
];

/// Check if a path looks like a MIME type (platform-agnostic, IANA standard).
fn is_mime_type(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.starts_with("application/")
        || lower.starts_with("text/")
        || lower.starts_with("image/")
        || lower.starts_with("audio/")
        || lower.starts_with("video/")
        || lower.starts_with("font/")
        || lower.starts_with("multipart/")
}

/// Check if a value matches any of the component's ignore patterns.
fn matches_ignore_pattern(value: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, value))
}

/// Extract all claims from a markdown document.
///
/// The `ignore_patterns` parameter allows components to filter out platform-specific
/// patterns (e.g., `/wp-json/*` for WordPress) without hardcoding them in core.
pub fn extract_claims(content: &str, doc_file: &str, ignore_patterns: &[String]) -> Vec<Claim> {
    let mut claims = Vec::new();

    // Track which positions we've already claimed to avoid duplicates
    let mut claimed_positions: Vec<(usize, usize)> = Vec::new();

    // Process line by line for line numbers
    for (line_idx, line) in content.lines().enumerate() {
        let line_num = line_idx + 1;

        // Skip lines that are inside code blocks (we'll handle those separately)
        if line.starts_with("```") {
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

                claimed_positions.push(pos);
                claims.push(Claim {
                    claim_type: ClaimType::FilePath,
                    value: path.to_string(),
                    doc_file: doc_file.to_string(),
                    line: line_num,
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

                claimed_positions.push(pos);
                claims.push(Claim {
                    claim_type: ClaimType::DirectoryPath,
                    value: path.to_string(),
                    doc_file: doc_file.to_string(),
                    line: line_num,
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
    fn test_extract_file_paths() {
        let content = "Check the file at `/inc/Engine/AI/Tools/BaseTool.php` for details.";
        let claims = extract_claims(content, "test.md", &[]);

        assert!(claims
            .iter()
            .any(|c| c.claim_type == ClaimType::FilePath
                && c.value == "/inc/Engine/AI/Tools/BaseTool.php"));
    }

    #[test]
    fn test_extract_file_paths_requires_separator() {
        // Files without path separator should NOT be extracted
        let content = "The `main.py` file is the entry point.";
        let claims = extract_claims(content, "test.md", &[]);

        assert!(!claims.iter().any(|c| c.value == "main.py"));
    }

    #[test]
    fn test_extract_directory_paths() {
        let content = "The tools are in `src/core/docs_audit/` directory.";
        let claims = extract_claims(content, "test.md", &[]);

        assert!(claims
            .iter()
            .any(|c| c.claim_type == ClaimType::DirectoryPath
                && c.value == "src/core/docs_audit/"));
    }

    #[test]
    fn test_skip_domain_patterns() {
        let content = "Visit `mysite.com/path/to/page.html` for documentation.";
        let claims = extract_claims(content, "test.md", &[]);

        // Should skip domain-like paths
        assert!(!claims.iter().any(|c| c.value.contains("mysite.com")));
    }

    #[test]
    fn test_extract_code_blocks() {
        let content = r#"
Example:
```php
function test() {
    return true;
}
```
"#;
        let claims = extract_claims(content, "test.md", &[]);

        assert!(claims
            .iter()
            .any(|c| c.claim_type == ClaimType::CodeExample));
    }

    #[test]
    fn test_skip_urls() {
        let content = "See https://example.com/path/to/file.html for details.";
        let claims = extract_claims(content, "test.md", &[]);

        assert!(claims.is_empty() || !claims.iter().any(|c| c.value.contains("https://")));
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
        let content = "The file type is `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`.";
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
    fn test_no_ignore_patterns_extracts_api_paths() {
        // Without ignore patterns, API-like paths ARE extracted
        let content = "The endpoint is at `/wp-json/datamachine/v1/events.json`.";
        let claims = extract_claims(content, "test.md", &[]);

        // With no patterns, this should be extracted as a file path
        assert!(claims.iter().any(|c| c.value.contains("wp-json")));
    }
}
