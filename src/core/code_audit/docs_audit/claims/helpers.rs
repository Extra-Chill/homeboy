//! helpers — extracted from claims.rs.

use glob_match::glob_match;
use regex::Regex;
use std::sync::LazyLock;
use super::Claim;
use super::classify_path_confidence;
use super::classify_class_confidence;
use super::ClaimConfidence;
use super::ClaimType;


/// Check if a path looks like a MIME type (platform-agnostic, IANA standard).
pub(crate) fn is_mime_type(path: &str) -> bool {
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
pub(crate) fn matches_ignore_pattern(value: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, value))
}

/// Check if a backslash-separated match is part of an OS filesystem path on the line.
///
/// Looks at characters before the regex match position to detect drive letters (`C:\`),
/// or other OS path indicators that mean this isn't a namespaced class reference.
pub(crate) fn is_os_path_context(line: &str, match_start: usize) -> bool {
    // Check if there's a drive letter + colon + backslash before the match
    // e.g., "C:\Users\<username>\AppData\Roaming"
    if match_start >= 2 {
        let prefix = &line[..match_start];
        // Look for X:\ pattern anywhere before the match
        if prefix.contains(":\\") || prefix.contains(":/") {
            return true;
        }
    }
    // Check if the line contains common OS path indicators
    let lower = line.to_lowercase();
    (lower.contains("c:\\") || lower.contains("c:/"))
        || (lower.contains("users\\") || lower.contains("users/"))
        || lower.contains("program files")
        || lower.contains("%appdata%")
        || lower.contains("$home")
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
pub(crate) fn is_domain_like(path: &str) -> bool {
    let lower = path.to_lowercase();
    // Check if any part of the path contains a domain extension
    // This catches both "mysite.com" and "mysite.com/path/to/file.html"
    DOMAIN_EXTENSIONS
        .iter()
        .any(|ext| lower.contains(&format!("{ext}/")) || lower.ends_with(ext))
}
