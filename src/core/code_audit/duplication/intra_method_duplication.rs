//! intra_method_duplication — extracted from duplication.rs.

use std::collections::HashMap;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;


/// Detect duplicated code blocks within the same method/function.
///
/// For each method in each file, extracts the method body from the file
/// content and uses a sliding window of `MIN_INTRA_BLOCK_LINES` normalized
/// lines. When the same window hash appears at two non-overlapping positions
/// within one method, it means a block of code was copy-pasted (merge
/// artifacts, copy-paste errors, etc.).
pub fn detect_intra_method_duplicates(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        if fp.content.is_empty() {
            continue;
        }

        let file_lines: Vec<&str> = fp.content.lines().collect();

        for method_name in &fp.methods {
            let Some((body_start, body_end)) = find_method_body(&file_lines, method_name) else {
                continue;
            };

            // Extract body lines (excluding the opening/closing brace lines)
            if body_start + 1 >= body_end {
                continue;
            }
            let body_lines: Vec<&str> = file_lines[body_start + 1..body_end].to_vec();

            if body_lines.len() < MIN_INTRA_BLOCK_LINES * 2 {
                // Body too short to contain two meaningful duplicate blocks
                continue;
            }

            // Build list of (original_body_index, normalized_text) for non-blank
            // non-comment lines
            let normalized: Vec<(usize, String)> = body_lines
                .iter()
                .enumerate()
                .filter_map(|(i, line)| {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || is_comment_only(trimmed) {
                        None
                    } else {
                        Some((i, normalize_line(trimmed)))
                    }
                })
                .collect();

            if normalized.len() < MIN_INTRA_BLOCK_LINES * 2 {
                continue;
            }

            // Hash each sliding window of MIN_INTRA_BLOCK_LINES consecutive
            // normalized lines. Store (hash, start_body_idx, end_body_idx).
            let mut window_hashes: Vec<(u64, usize, usize)> = Vec::new();

            for win_start in 0..=normalized.len() - MIN_INTRA_BLOCK_LINES {
                let win_end = win_start + MIN_INTRA_BLOCK_LINES;
                let mut hasher = std::hash::DefaultHasher::new();
                for (_, norm_line) in &normalized[win_start..win_end] {
                    std::hash::Hash::hash(norm_line, &mut hasher);
                }
                let hash = std::hash::Hasher::finish(&hasher);

                let orig_start = normalized[win_start].0;
                let orig_end = normalized[win_end - 1].0;

                window_hashes.push((hash, orig_start, orig_end));
            }

            // Group by hash, look for non-overlapping pairs
            let mut hash_positions: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
            for (hash, start, end) in &window_hashes {
                hash_positions
                    .entry(*hash)
                    .or_default()
                    .push((*start, *end));
            }

            let mut reported = false;

            for positions in hash_positions.values() {
                if reported || positions.len() < 2 {
                    continue;
                }

                let first = positions[0];
                for other in &positions[1..] {
                    // Non-overlapping: second block starts after first block ends
                    if other.0 <= first.1 {
                        continue;
                    }

                    // Extend the match: keep sliding forward while lines match
                    let first_norm_idx = normalized
                        .iter()
                        .position(|(i, _)| *i == first.0)
                        .unwrap_or(0);
                    let other_norm_idx = normalized
                        .iter()
                        .position(|(i, _)| *i == other.0)
                        .unwrap_or(0);

                    let mut match_len = MIN_INTRA_BLOCK_LINES;
                    while first_norm_idx + match_len < normalized.len()
                        && other_norm_idx + match_len < normalized.len()
                        && first_norm_idx + match_len < other_norm_idx
                    {
                        if normalized[first_norm_idx + match_len].1
                            == normalized[other_norm_idx + match_len].1
                        {
                            match_len += 1;
                        } else {
                            break;
                        }
                    }

                    // Convert body-relative line numbers to 1-indexed file lines
                    let first_file_line = body_start + 1 + first.0 + 1;
                    let other_file_line = body_start + 1 + other.0 + 1;

                    findings.push(Finding {
                        convention: "intra-method-duplication".to_string(),
                        severity: Severity::Warning,
                        file: fp.relative_path.clone(),
                        description: format!(
                            "Duplicated block in `{}` — {} identical lines at line {} and line {}",
                            method_name, match_len, first_file_line, other_file_line
                        ),
                        suggestion: format!(
                            "Function `{}` contains a duplicated code block ({} lines). \
                             This is often a merge artifact or copy-paste error. \
                             Remove the duplicate or extract shared logic.",
                            method_name, match_len
                        ),
                        kind: AuditFinding::IntraMethodDuplicate,
                    });
                    reported = true;
                    break;
                }

                if reported {
                    break;
                }
            }
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

/// Find the body of a method/function in the file lines.
///
/// Returns `(open_brace_line, close_brace_line)` — the line indices of the
/// opening and closing braces. Searches for `function <name>` or `fn <name>`.
pub(crate) fn find_method_body(lines: &[&str], method_name: &str) -> Option<(usize, usize)> {
    let fn_pattern_php = format!("function {}", method_name);
    let fn_pattern_rust = format!("fn {}", method_name);

    let mut start_line = None;
    for (i, line) in lines.iter().enumerate() {
        if line.contains(&fn_pattern_php) || line.contains(&fn_pattern_rust) {
            start_line = Some(i);
            break;
        }
    }

    let start = start_line?;

    // Find opening brace from the function declaration line
    let mut brace_line = None;
    for (offset, line) in lines[start..].iter().enumerate() {
        if line.contains('{') {
            brace_line = Some(start + offset);
            break;
        }
    }

    let open_line = brace_line?;

    // Track brace depth to find closing brace
    let mut depth = 0i32;
    let mut found_open = false;
    for (i, line) in lines[open_line..].iter().enumerate() {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if found_open && depth == 0 {
            return Some((open_line, open_line + i));
        }
    }

    None
}

/// Check if a line is comment-only (PHP, Rust, or shell style).
pub(crate) fn is_comment_only(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('#')
}

/// Normalize a line for hashing: collapse whitespace, lowercase.
pub(crate) fn normalize_line(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
