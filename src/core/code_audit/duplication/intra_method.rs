use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use super::find_method_body;

// ============================================================================
// Intra-Method Duplication Detection
// ============================================================================

/// Minimum number of non-blank, non-comment lines for a block to be
/// considered meaningful. Blocks shorter than this are too trivial to flag.
const MIN_INTRA_BLOCK_LINES: usize = 5;

/// Detect duplicated code blocks within the same method/function.
///
/// For each method in each file, extracts the method body from the file
/// content and uses a sliding window of `MIN_INTRA_BLOCK_LINES` normalized
/// lines. When the same window hash appears at two non-overlapping positions
/// within one method, it means a block of code was copy-pasted (merge
/// artifacts, copy-paste errors, etc.).
pub(crate) fn detect_intra_method_duplicates(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
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
                let mut hasher = DefaultHasher::new();
                for (_, norm_line) in &normalized[win_start..win_end] {
                    Hash::hash(norm_line, &mut hasher);
                }
                let hash = Hasher::finish(&hasher);

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
            let mut suppressed_ranges: Vec<(usize, usize)> = Vec::new();

            let mut duplicate_windows: Vec<&Vec<(usize, usize)>> =
                hash_positions.values().collect();
            duplicate_windows.sort_by_key(|positions| positions.first().copied());

            for positions in duplicate_windows {
                if reported || positions.len() < 2 {
                    continue;
                }

                let first = positions[0];
                for other in &positions[1..] {
                    // Non-overlapping: second block starts after first block ends
                    if other.0 <= first.1 {
                        continue;
                    }

                    if is_inside_suppressed_range(first, &suppressed_ranges)
                        || is_inside_suppressed_range(*other, &suppressed_ranges)
                    {
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

                    // Suppress structural-syntax-only windows. Match-arm tails
                    // (`},`, `)?;`, `Ok((...))`, closing brace, bare-identifier
                    // struct-literal fields) repeat naturally across sibling
                    // dispatch branches in `run_*` functions — they're not
                    // merge artifacts or copy-paste, they're Rust syntax.
                    // A block is worth flagging only if it contains at least
                    // one logic-bearing line.
                    if is_structural_syntax_only(&normalized, first_norm_idx, match_len) {
                        continue;
                    }

                    if is_branch_shape_repetition(&body_lines, first, *other, match_len)
                        || is_low_information_literal_or_error_block(
                            &normalized,
                            first_norm_idx,
                            match_len,
                            MIN_INTRA_BLOCK_LINES,
                        )
                    {
                        suppressed_ranges
                            .push((first.0, normalized[first_norm_idx + match_len - 1].0));
                        suppressed_ranges
                            .push((other.0, normalized[other_norm_idx + match_len - 1].0));
                        continue;
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

/// Check if a line is comment-only (PHP, Rust, or shell style).
fn is_comment_only(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('#')
}

/// Normalize a line for hashing: collapse whitespace, lowercase.
fn normalize_line(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Return true when the window `normalized[start..start+len]` is pure
/// syntactic scaffolding with no logic-bearing content.
///
/// A window is scaffolding when **every** line is one of:
/// - pure punctuation closers (`}`, `},`, `)?;`, etc.)
/// - a single identifier, optionally trailed by `,` (struct-literal or
///   destructuring fields)
/// - common match-arm glue (`=> {`, `} => {`)
///
/// **and** none of the lines contain logic signals (`=`, `let `, `if `,
/// `for `, `while `, `match `, `return`, or a function-call shape
/// `foo(` / `foo::bar(`). If a single line in the window carries any
/// logic signal, the window is not scaffolding and gets flagged normally.
///
/// Match-arm tails (`)?;`, `Ok((x, 0))`, struct-literal closers) repeated
/// across sibling arms of a dispatch `match` are structural noise, not
/// duplication — this filter stops them from tripping the detector.
fn is_structural_syntax_only(normalized: &[(usize, String)], start: usize, len: usize) -> bool {
    let end = (start + len).min(normalized.len());
    if start >= end {
        return false;
    }
    let window = &normalized[start..end];

    // If any line in the window looks logical, window is not scaffolding.
    if window.iter().any(|(_, line)| has_logic_signal(line)) {
        return false;
    }

    // Every line must match a known scaffolding shape.
    window.iter().all(|(_, line)| is_scaffolding_line(line))
}

/// Lines that look like they do real work: assignment, control flow, or
/// a user function call that isn't a dispatch-return wrapper.
pub(super) fn has_logic_signal(normalized: &str) -> bool {
    let t = normalized.trim();

    // Assignment or `let` binding.
    if t.contains(" = ") || t.starts_with("let ") {
        return true;
    }

    // Control flow keywords (normalized to lowercase by the caller).
    for kw in ["if ", "for ", "while ", "match ", "return ", "loop ", "?;"] {
        if t.contains(kw) && !matches!(t, ")?;" | "})?;") {
            return true;
        }
    }

    // Function / method calls that aren't bare dispatch-return wrappers.
    // `ok(...)`, `err(...)`, `some(...)`, `none` by themselves are scaffolding
    // (return-tail on a match arm); anything else with parens is real work.
    if t.contains('(') {
        let before_paren = t.split('(').next().unwrap_or("");
        let head = before_paren.trim_end_matches(':').trim_end_matches(':');
        let head = head.trim();
        let is_return_wrapper = matches!(head, "ok" | "err" | "some")
            || head.ends_with(" ok")
            || head.ends_with(" err")
            || head.ends_with(" some");
        if !is_return_wrapper {
            return true;
        }
    }

    false
}

/// Does this normalized line match a known scaffolding shape?
pub(super) fn is_scaffolding_line(normalized: &str) -> bool {
    let t = normalized.trim();
    if t.is_empty() {
        return true;
    }

    // Pure-punctuation closers: `}`, `},`, `)?;`, `))`, etc.
    if t.chars()
        .all(|c| matches!(c, '}' | ')' | '?' | ';' | ',' | '('))
    {
        return true;
    }

    // Bare identifier (optionally trailing comma) — struct-literal field or
    // destructure.
    let core = t.trim_end_matches(',');
    if !core.is_empty() && core.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return true;
    }

    // Dispatch-return tails: `ok(...)`, `err(...)`, `some(...)`, `none`
    // (optionally with trailing `?`, `;`, `,`).
    let core = t.trim_end_matches([',', ';', '?']);
    if core == "none"
        || core.starts_with("ok(")
        || core.starts_with("err(")
        || core.starts_with("some(")
    {
        return true;
    }

    // Match-arm glue.
    if t.ends_with("=> {") || t == "} => {" || t == "_ => {" {
        return true;
    }

    false
}

/// Repeated blocks in sibling `if` / `else if` / `else` arms are usually local
/// branch shape, not high-confidence copy/paste. Keep this deliberately narrow:
/// long blocks can still indicate real duplication, and adjacent repeated logic
/// outside branch arms is still reported.
fn is_branch_shape_repetition(
    body_lines: &[&str],
    first: (usize, usize),
    other: (usize, usize),
    match_len: usize,
) -> bool {
    if match_len > 12 || first.1 >= other.0 || other.0 > body_lines.len() {
        return false;
    }

    body_lines[first.1 + 1..other.0]
        .iter()
        .any(|line| is_branch_separator(line.trim()))
}

fn is_inside_suppressed_range(candidate: (usize, usize), ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(start, end)| candidate.0 >= *start && candidate.1 <= *end)
}

fn is_branch_separator(trimmed: &str) -> bool {
    trimmed.starts_with("} else")
        || trimmed.starts_with("else ")
        || trimmed.starts_with("elseif ")
        || trimmed.starts_with("} elseif")
        || trimmed.ends_with("=> {")
        || trimmed.starts_with("} => {")
}

/// Suppress low-information literal/envelope repeats: DTO tails full of
/// `None`/`Default::default()` and repeated error constructors. These are common
/// review-noise patterns where extraction usually hides branch intent.
pub(super) fn is_low_information_literal_or_error_block(
    normalized: &[(usize, String)],
    start: usize,
    len: usize,
    min_block_lines: usize,
) -> bool {
    let end = (start + len).min(normalized.len());
    if start >= end {
        return false;
    }

    let window = &normalized[start..end];
    let low_info_lines = window
        .iter()
        .filter(|(_, line)| is_low_information_literal_or_error_line(line))
        .count();

    low_info_lines >= min_block_lines && low_info_lines * 100 / window.len() >= 80
}

fn is_low_information_literal_or_error_line(normalized: &str) -> bool {
    let t = normalized.trim().trim_end_matches(',');

    if t.is_empty() || is_scaffolding_line(t) {
        return true;
    }

    if t == "0" || t == "..default::default()" {
        return true;
    }

    if is_neutral_struct_field(t) {
        return true;
    }

    if is_error_envelope_line(t) {
        return true;
    }

    if is_simple_argument_line(t) {
        return true;
    }

    false
}

fn is_simple_argument_line(line: &str) -> bool {
    let mut value = line.trim();
    value = value.strip_prefix("&mut ").unwrap_or(value);
    value = value.strip_prefix('&').unwrap_or(value).trim();

    if is_simple_identifier_path(value) {
        return true;
    }

    if value.ends_with(".clone()") || value.ends_with(".to_string()") {
        return true;
    }

    if let Some((left, right)) = value.split_once(" + ") {
        return is_simple_identifier_path(left.trim()) && right.trim().parse::<u64>().is_ok();
    }

    false
}

fn is_simple_identifier_path(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
        && value.chars().any(|c| c.is_ascii_alphabetic() || c == '_')
}

fn is_neutral_struct_field(line: &str) -> bool {
    let Some((_field, value)) = line.split_once(':') else {
        return false;
    };
    let value = value.trim();

    value == "none"
        || value == "default::default()"
        || value == "false"
        || value == "0"
        || value.ends_with(".clone()")
        || value.ends_with(".to_string()")
        || value.starts_with("some(")
        || is_simple_argument_line(value)
}

fn is_error_envelope_line(line: &str) -> bool {
    line.contains("error::")
        || line.contains("::error")
        || line.contains("internal_io(")
        || line.starts_with("format!(")
        || line.starts_with("some(")
}
