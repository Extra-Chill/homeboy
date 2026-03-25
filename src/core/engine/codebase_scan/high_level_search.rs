//! high_level_search — extracted from codebase_scan.rs.

use std::path::{Path, PathBuf};
use super::Match;
use super::SearchMode;
use super::ScanConfig;


/// Search for a term across all files in a directory tree.
///
/// Returns matches with file path, line number, column, matched text, and context.
pub fn search(root: &Path, term: &str, mode: &SearchMode, config: &ScanConfig) -> Vec<Match> {
    let files = walk_files(root, config);
    let mut results = Vec::new();

    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        let relative = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        for (line_num, line) in content.lines().enumerate() {
            match mode {
                SearchMode::Boundary => {
                    for pos in find_boundary_matches(line, term) {
                        results.push(Match {
                            file: relative.clone(),
                            line: line_num + 1,
                            column: pos + 1,
                            matched: term.to_string(),
                            context: line.to_string(),
                        });
                    }
                }
                SearchMode::Literal => {
                    for pos in find_literal_matches(line, term) {
                        results.push(Match {
                            file: relative.clone(),
                            line: line_num + 1,
                            column: pos + 1,
                            matched: term.to_string(),
                            context: line.to_string(),
                        });
                    }
                }
                SearchMode::CaseInsensitive => {
                    for (pos, actual) in find_case_insensitive_matches(line, term) {
                        results.push(Match {
                            file: relative.clone(),
                            line: line_num + 1,
                            column: pos + 1,
                            matched: actual,
                            context: line.to_string(),
                        });
                    }
                }
            }
        }
    }

    results
}

/// Discover the actual casing of a term in the codebase.
///
/// Given a term like `WpAgent`, searches case-insensitively and returns
/// all distinct casings found (e.g., `WPAgent`). Useful for variant discovery
/// when a generated variant has 0 boundary matches.
pub fn discover_casing(root: &Path, term: &str, config: &ScanConfig) -> Vec<(String, usize)> {
    let matches = search(root, term, &SearchMode::CaseInsensitive, config);

    // Group by actual matched text, count occurrences
    let mut casing_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for m in &matches {
        *casing_counts.entry(m.matched.clone()).or_insert(0) += 1;
    }

    let mut result: Vec<(String, usize)> = casing_counts.into_iter().collect();
    result.sort_by(|a, b| b.1.cmp(&a.1)); // Most frequent first
    result
}
