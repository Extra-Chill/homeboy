//! Repeated inline array literal shape detection.
//!
//! Finds repeated associative array literals (same ordered keys + same value
//! kinds) across a codebase. When the same shape occurs many times, it is a
//! candidate for extraction into a helper constructor (e.g. an
//! `error_envelope($error, $message)` helper for the ubiquitous
//! `['success' => false, 'error' => $x, 'message' => $y]` shape).
//!
//! PHP-first. The detector recognizes two literal syntaxes:
//!
//! - Short array: `[ 'key' => value, ... ]`
//! - Long array:  `array( 'key' => value, ... )`
//!
//! Positional/list-only arrays (e.g. `['a', 'b']`) are intentionally skipped —
//! they are not interesting for helper extraction.
//!
//! Parsing strategy: a tolerant, character-level scanner that tracks string
//! state and bracket depth. When the scanner enters a top-level array literal
//! it accumulates key/value pairs between matching delimiters and then
//! normalizes the literal to a shape signature:
//!
//! ```text
//! Vec<(String /* key */, ValueKind)>
//! ```
//!
//! Nested array literals inside a value are treated as a single
//! `ValueKind::Expression` token — the detector does not recurse on the first
//! pass.

use std::collections::HashMap;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

/// Minimum number of occurrences of a shape to emit a finding.
const MIN_OCCURRENCES: usize = 20;

/// Estimated lines of code per literal occurrence (one line per key/value pair
/// plus opener/closer). Used for the LOC-reduction estimate in the finding
/// description.
const AVG_LITERAL_LOC: usize = 4;

/// Estimated helper size (function definition + body).
const HELPER_LOC: usize = 6;

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_repeated_literal_shapes(fingerprints)
}

/// Classification of a literal value's kind — concrete values are discarded,
/// only the kind is retained so shapes match regardless of actual payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ValueKind {
    Bool,
    Int,
    String,
    Null,
    Variable,
    Expression,
}

impl ValueKind {
    fn as_str(&self) -> &'static str {
        match self {
            ValueKind::Bool => "bool",
            ValueKind::Int => "int",
            ValueKind::String => "string",
            ValueKind::Null => "null",
            ValueKind::Variable => "var",
            ValueKind::Expression => "expr",
        }
    }
}

/// Ordered shape signature: (key, value kind) pairs in source order.
type Shape = Vec<(String, ValueKind)>;

/// One occurrence of a literal shape at a specific source site. The file path
/// is enough for the current detector; line numbers are deferred to the fixer
/// (which will re-scan the file) to keep this pass cheap on very large
/// codebases.
#[derive(Debug, Clone)]
struct Occurrence {
    file: String,
}

fn detect_repeated_literal_shapes(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    // shape → all occurrences across all scanned files
    let mut shape_occurrences: HashMap<Shape, Vec<Occurrence>> = HashMap::new();

    for fp in fingerprints {
        if !is_php(&fp.relative_path) {
            continue;
        }
        if super::walker::is_test_path(&fp.relative_path) {
            continue;
        }

        for shape in extract_literal_shapes(&fp.content) {
            shape_occurrences
                .entry(shape)
                .or_default()
                .push(Occurrence {
                    file: fp.relative_path.clone(),
                });
        }
    }

    let mut findings = Vec::new();

    for (shape, occurrences) in &shape_occurrences {
        if occurrences.len() < MIN_OCCURRENCES {
            continue;
        }

        // Count occurrences per file and pick the top 3 files for the finding.
        let mut file_counts: HashMap<&str, usize> = HashMap::new();
        for occ in occurrences {
            *file_counts.entry(occ.file.as_str()).or_insert(0) += 1;
        }
        let mut top_files: Vec<(&str, usize)> = file_counts.iter().map(|(f, c)| (*f, *c)).collect();
        top_files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        top_files.truncate(3);

        let shape_display = format_shape(shape);
        let total = occurrences.len();

        // LOC reduction estimate: if each site averages AVG_LITERAL_LOC and the
        // helper costs HELPER_LOC once, extraction saves roughly
        //   total * (AVG_LITERAL_LOC - 1)   (the call site keeps ~1 line)
        // minus the helper's own weight.
        let estimated_reduction = total
            .saturating_mul(AVG_LITERAL_LOC.saturating_sub(1))
            .saturating_sub(HELPER_LOC);

        let top_files_display: Vec<String> = top_files
            .iter()
            .map(|(f, c)| format!("{} ({})", f, c))
            .collect();

        // Pick a stable "representative" file for the Finding.file field —
        // the top-occurrence file, so the finding anchors where the pattern
        // is most concentrated.
        let anchor_file = top_files
            .first()
            .map(|(f, _)| (*f).to_string())
            .unwrap_or_else(|| "<unknown>".to_string());

        let helper_hint = suggest_helper_name(shape);

        findings.push(Finding {
            convention: "repeated_literal_shape".to_string(),
            severity: Severity::Info,
            file: anchor_file,
            description: format!(
                "Repeated literal shape [{}] appears {} time(s); top files: {}; estimated LOC reduction: ~{}",
                shape_display,
                total,
                top_files_display.join(", "),
                estimated_reduction
            ),
            suggestion: format!(
                "Extract a helper (e.g. `{}`) that returns this shape and replace the {} inline literals with calls",
                helper_hint, total
            ),
            kind: AuditFinding::RepeatedLiteralShape,
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn is_php(path: &str) -> bool {
    path.ends_with(".php")
}

fn format_shape(shape: &Shape) -> String {
    shape
        .iter()
        .map(|(k, v)| format!("'{}' => <{}>", k, v.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Suggest a helper name derived from the shape's key set.
///
/// Purely heuristic — the output is a hint in the finding's suggestion string,
/// not a contract. Recognizes common shapes (`success`/`error`/`message` →
/// `error_envelope`) and falls back to a concatenation of the keys.
fn suggest_helper_name(shape: &Shape) -> String {
    let keys: Vec<&str> = shape.iter().map(|(k, _)| k.as_str()).collect();
    let key_set: std::collections::HashSet<&str> = keys.iter().copied().collect();

    if key_set.contains("success") && (key_set.contains("error") || key_set.contains("message")) {
        return "error_envelope(...)".to_string();
    }
    if key_set.contains("data") && key_set.contains("success") {
        return "success_envelope(...)".to_string();
    }

    let joined = keys.iter().take(3).copied().collect::<Vec<_>>().join("_");
    format!("build_{}(...)", joined)
}

// ============================================================================
// Literal extraction
// ============================================================================

/// Extract all top-level associative array literal shapes from a PHP source
/// string. Positional arrays and non-associative literals are skipped.
fn extract_literal_shapes(content: &str) -> Vec<Shape> {
    let bytes = content.as_bytes();
    let mut results = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        // Fast-skip strings and comments at the outer scan level so we do not
        // accidentally open an array literal inside a quoted string.
        if let Some(next) = skip_string_or_comment(bytes, i) {
            i = next;
            continue;
        }

        // `[` → short array literal candidate.
        if bytes[i] == b'[' && !looks_like_subscript(bytes, i) {
            if let Some((shape, end)) = parse_array_literal(bytes, i + 1, b']') {
                if !shape.is_empty() {
                    results.push(shape);
                }
                i = end;
                continue;
            }
        }

        // `array(` → long array literal candidate.
        if bytes[i] == b'a' && starts_with_ci(bytes, i, b"array") {
            let after = i + 5;
            // Skip whitespace between `array` and `(`.
            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' && is_ident_boundary(bytes, i) {
                if let Some((shape, end)) = parse_array_literal(bytes, j + 1, b')') {
                    if !shape.is_empty() {
                        results.push(shape);
                    }
                    i = end;
                    continue;
                }
            }
        }

        i += 1;
    }

    results
}

/// Starting just after the opening delimiter, read until the matching closer
/// (`]` or `)`), splitting the top-level content on commas and parsing each
/// segment as a `key => value` pair.
///
/// Returns `Some((shape, index_past_closer))` on success; `None` if the
/// literal is malformed (mismatched brackets, EOF before closer).
///
/// If any segment lacks a `=>`, the literal is treated as positional/list and
/// an empty `Shape` is returned (caller then skips it).
fn parse_array_literal(bytes: &[u8], start: usize, closer: u8) -> Option<(Shape, usize)> {
    let mut depth = 0i32;
    let mut segment_start = start;
    let mut segments: Vec<(usize, usize)> = Vec::new();
    let mut i = start;

    while i < bytes.len() {
        if let Some(next) = skip_string_or_comment(bytes, i) {
            i = next;
            continue;
        }

        let b = bytes[i];
        match b {
            b'[' | b'(' | b'{' => {
                depth += 1;
                i += 1;
            }
            b']' | b')' | b'}' => {
                if depth == 0 {
                    if b != closer {
                        // Mismatched closer — malformed literal.
                        return None;
                    }
                    // Close out the final segment.
                    if segment_start < i {
                        segments.push((segment_start, i));
                    }
                    return Some((segments_to_shape(bytes, &segments), i + 1));
                }
                depth -= 1;
                i += 1;
            }
            b',' if depth == 0 => {
                segments.push((segment_start, i));
                segment_start = i + 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    None
}

/// Convert the list of top-level segments into a Shape. Every segment must be
/// a `key => value` pair with a string-literal key — otherwise return an empty
/// shape to signal "not an associative literal, skip".
fn segments_to_shape(bytes: &[u8], segments: &[(usize, usize)]) -> Shape {
    let mut shape = Shape::new();

    for (start, end) in segments {
        let seg = &bytes[*start..*end];
        let seg_trimmed = trim_ascii(seg);
        if seg_trimmed.is_empty() {
            // Trailing comma produces an empty final segment — ignore.
            continue;
        }

        // Find `=>` at top level within this segment.
        let Some(arrow_pos) = find_top_level_arrow(seg_trimmed) else {
            // No arrow → positional element → not associative, abort.
            return Shape::new();
        };

        let key_bytes = trim_ascii(&seg_trimmed[..arrow_pos]);
        let value_bytes = trim_ascii(&seg_trimmed[arrow_pos + 2..]);

        let Some(key) = parse_string_literal_key(key_bytes) else {
            // Keys that aren't simple string literals (e.g. constants,
            // expressions) are out of scope for the first-pass detector.
            return Shape::new();
        };

        let kind = classify_value(value_bytes);
        shape.push((key, kind));
    }

    shape
}

/// Locate `=>` at bracket-depth zero within the segment's byte slice.
fn find_top_level_arrow(bytes: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(next) = skip_string_or_comment(bytes, i) {
            i = next;
            continue;
        }
        let b = bytes[i];
        match b {
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth -= 1,
            b'=' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Extract the inner text of a single- or double-quoted string literal.
/// Returns `None` if the bytes do not form a quoted string.
fn parse_string_literal_key(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 {
        return None;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if (first != b'\'' && first != b'"') || first != last {
        return None;
    }
    let inner = &bytes[1..bytes.len() - 1];
    // Reject keys that contain an unescaped matching quote in the middle — the
    // literal is malformed and we don't want to mis-parse it.
    let s = std::str::from_utf8(inner).ok()?;
    Some(s.to_string())
}

/// Classify a value expression by its leading structure.
fn classify_value(bytes: &[u8]) -> ValueKind {
    let trimmed = trim_ascii(bytes);
    if trimmed.is_empty() {
        return ValueKind::Expression;
    }

    let first = trimmed[0];

    // Variable: `$foo`
    if first == b'$' {
        // If the variable is followed by an operator (->, [, etc), it's an
        // expression; bare `$foo` is a variable.
        return if is_bare_variable(trimmed) {
            ValueKind::Variable
        } else {
            ValueKind::Expression
        };
    }

    // String literal (only if the quote terminates the whole segment).
    if (first == b'\'' || first == b'"') && ends_with_matching_quote(trimmed, first) {
        return ValueKind::String;
    }

    // Integer literal: optional sign + digits only.
    if is_integer_literal(trimmed) {
        return ValueKind::Int;
    }

    // Keywords: case-insensitive `true`, `false`, `null`.
    if eq_ci(trimmed, b"true") || eq_ci(trimmed, b"false") {
        return ValueKind::Bool;
    }
    if eq_ci(trimmed, b"null") {
        return ValueKind::Null;
    }

    ValueKind::Expression
}

fn is_bare_variable(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes[0] != b'$' {
        return false;
    }
    for b in &bytes[1..] {
        if !(b.is_ascii_alphanumeric() || *b == b'_') {
            return false;
        }
    }
    bytes.len() > 1
}

fn ends_with_matching_quote(bytes: &[u8], quote: u8) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    if bytes[bytes.len() - 1] != quote {
        return false;
    }
    // Walk the string to ensure the closing quote is the literal's terminator
    // (and nothing trails it).
    let mut i = 1;
    while i < bytes.len() - 1 {
        if bytes[i] == b'\\' && i + 1 < bytes.len() - 1 {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            // Premature closer before end of segment — expression, not a bare
            // string literal.
            return false;
        }
        i += 1;
    }
    true
}

fn is_integer_literal(bytes: &[u8]) -> bool {
    let mut start = 0;
    if bytes.first() == Some(&b'-') || bytes.first() == Some(&b'+') {
        start = 1;
    }
    if start >= bytes.len() {
        return false;
    }
    bytes[start..].iter().all(|b| b.is_ascii_digit())
}

// ============================================================================
// String / comment skipping
// ============================================================================

/// If the cursor is at the start of a string or comment, return the index just
/// past the end of that span. Otherwise return `None`.
fn skip_string_or_comment(bytes: &[u8], i: usize) -> Option<usize> {
    if i >= bytes.len() {
        return None;
    }
    let b = bytes[i];

    // Line comments: `//` and `#` (the latter only when not followed by `[` to
    // avoid colliding with PHP 8 attribute syntax `#[...]`).
    if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
        return Some(skip_to_eol(bytes, i + 2));
    }
    if b == b'#' && bytes.get(i + 1) != Some(&b'[') {
        return Some(skip_to_eol(bytes, i + 1));
    }

    // Block comments: `/* ... */`
    if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
        let mut j = i + 2;
        while j + 1 < bytes.len() {
            if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                return Some(j + 2);
            }
            j += 1;
        }
        return Some(bytes.len());
    }

    // String literals.
    if b == b'\'' || b == b'"' {
        return Some(skip_string(bytes, i, b));
    }

    None
}

fn skip_to_eol(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_string(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if b == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

// ============================================================================
// Helpers
// ============================================================================

/// A `[` is a subscript (not an array literal) when the IMMEDIATELY preceding
/// byte (no whitespace between) is an identifier character, a closing
/// `)`/`]`/`}`, or a `$` — e.g. `$foo[0]`, `bar()[0]`.
///
/// When separated by whitespace, `[` is treated as an array literal opener
/// (e.g. `return ['a' => 1]`). This is imperfect but correct for the common
/// associative-literal patterns the detector targets.
fn looks_like_subscript(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return false;
    }
    let b = bytes[i - 1];
    b.is_ascii_alphanumeric() || b == b'_' || b == b')' || b == b']' || b == b'}' || b == b'$'
}

/// True when `bytes[i..]` begins with `needle` (case-insensitive ASCII).
fn starts_with_ci(bytes: &[u8], i: usize, needle: &[u8]) -> bool {
    if i + needle.len() > bytes.len() {
        return false;
    }
    for (k, nb) in needle.iter().enumerate() {
        if bytes[i + k].to_ascii_lowercase() != nb.to_ascii_lowercase() {
            return false;
        }
    }
    true
}

/// True if the byte at `i` is at an identifier boundary — i.e. the preceding
/// byte is not itself part of an identifier. Prevents matching `my_array(` as
/// the keyword `array(`.
fn is_ident_boundary(bytes: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = bytes[i - 1];
    !(prev.is_ascii_alphanumeric() || prev == b'_')
}

fn eq_ci(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.len() == needle.len() && starts_with_ci(bytes, 0, needle)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;

    fn fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn classifies_scalar_value_kinds() {
        assert_eq!(classify_value(b"false"), ValueKind::Bool);
        assert_eq!(classify_value(b"TRUE"), ValueKind::Bool);
        assert_eq!(classify_value(b"null"), ValueKind::Null);
        assert_eq!(classify_value(b"42"), ValueKind::Int);
        assert_eq!(classify_value(b"-7"), ValueKind::Int);
        assert_eq!(classify_value(b"'hello'"), ValueKind::String);
        assert_eq!(classify_value(b"\"world\""), ValueKind::String);
        assert_eq!(classify_value(b"$foo"), ValueKind::Variable);
        assert_eq!(classify_value(b"$foo->bar"), ValueKind::Expression);
        assert_eq!(classify_value(b"sprintf('x')"), ValueKind::Expression);
        assert_eq!(classify_value(b"3.14"), ValueKind::Expression);
    }

    #[test]
    fn extracts_short_array_shape() {
        let src = "<?php\n$x = ['success' => false, 'message' => $m];\n";
        let shapes = extract_literal_shapes(src);
        assert_eq!(shapes.len(), 1);
        let shape = &shapes[0];
        assert_eq!(shape.len(), 2);
        assert_eq!(shape[0], ("success".to_string(), ValueKind::Bool));
        assert_eq!(shape[1], ("message".to_string(), ValueKind::Variable));
    }

    #[test]
    fn extracts_long_array_shape() {
        let src = "<?php\nreturn array( 'success' => false, 'error' => $err );\n";
        let shapes = extract_literal_shapes(src);
        assert_eq!(shapes.len(), 1);
        let shape = &shapes[0];
        assert_eq!(shape[0], ("success".to_string(), ValueKind::Bool));
        assert_eq!(shape[1], ("error".to_string(), ValueKind::Variable));
    }

    #[test]
    fn skips_positional_arrays() {
        let src = "<?php\n$x = ['a', 'b', 'c'];\n";
        let shapes = extract_literal_shapes(src);
        assert!(
            shapes.is_empty(),
            "positional arrays must not produce a shape, got {:?}",
            shapes
        );
    }

    #[test]
    fn ignores_subscripts() {
        // $foo[0] must not be parsed as a literal.
        let src = "<?php\n$v = $foo[0] + $bar['key'];\n";
        let shapes = extract_literal_shapes(src);
        assert!(shapes.is_empty(), "got unexpected shapes: {:?}", shapes);
    }

    #[test]
    fn ignores_array_inside_string() {
        let src = "<?php\n$s = \"['success' => false]\";\n";
        let shapes = extract_literal_shapes(src);
        assert!(shapes.is_empty());
    }

    #[test]
    fn ignores_array_inside_line_comment() {
        let src = "<?php\n// ['success' => false]\n$x = 1;\n";
        let shapes = extract_literal_shapes(src);
        assert!(shapes.is_empty());
    }

    #[test]
    fn handles_trailing_comma() {
        let src = "<?php\nreturn ['a' => 1, 'b' => 2,];\n";
        let shapes = extract_literal_shapes(src);
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].len(), 2);
    }

    #[test]
    fn nested_literal_counts_once_at_top_level() {
        // The outer literal is associative; its value contains a nested array.
        // The nested array is treated as ValueKind::Expression (not recursed).
        let src = "<?php\n$x = ['meta' => ['a' => 1], 'ok' => true];\n";
        let shapes = extract_literal_shapes(src);
        assert_eq!(shapes.len(), 1, "expected single top-level shape");
        let shape = &shapes[0];
        assert_eq!(shape.len(), 2);
        assert_eq!(shape[0].0, "meta");
        assert_eq!(shape[0].1, ValueKind::Expression);
        assert_eq!(shape[1], ("ok".to_string(), ValueKind::Bool));
    }

    #[test]
    fn detects_shape_repeated_above_threshold() {
        // 25 occurrences of the same shape across 3 files → exactly one finding.
        let mut files: Vec<FileFingerprint> = Vec::new();
        for i in 0..3 {
            let mut body = String::from("<?php\n");
            let per_file = if i == 0 { 9 } else { 8 };
            for _ in 0..per_file {
                body.push_str("return ['success' => false, 'error' => $e, 'message' => $m];\n");
            }
            files.push(fp(&format!("inc/Abilities/F{}.php", i), &body));
        }
        let refs: Vec<&FileFingerprint> = files.iter().collect();
        let findings = detect_repeated_literal_shapes(&refs);
        assert_eq!(
            findings.len(),
            1,
            "expected exactly one finding, got {:?}",
            findings
        );
        let f = &findings[0];
        assert_eq!(f.kind, AuditFinding::RepeatedLiteralShape);
        assert_eq!(f.severity, Severity::Info);
        assert!(f.description.contains("'success'"));
        assert!(f.description.contains("25"));
    }

    #[test]
    fn ignores_shape_below_threshold() {
        // 10 occurrences — below MIN_OCCURRENCES (20).
        let mut body = String::from("<?php\n");
        for _ in 0..10 {
            body.push_str("return ['success' => false, 'error' => $e];\n");
        }
        let file = fp("inc/Abilities/Small.php", &body);
        let findings = detect_repeated_literal_shapes(&[&file]);
        assert!(
            findings.is_empty(),
            "below-threshold shapes must not fire, got {:?}",
            findings
        );
    }

    #[test]
    fn ignores_positional_only_arrays_at_scale() {
        // 30 positional arrays must never produce a finding.
        let mut body = String::from("<?php\n");
        for _ in 0..30 {
            body.push_str("$x = ['a', 'b', 'c'];\n");
        }
        let file = fp("inc/Abilities/List.php", &body);
        let findings = detect_repeated_literal_shapes(&[&file]);
        assert!(
            findings.is_empty(),
            "positional arrays must never fire, got {:?}",
            findings
        );
    }

    #[test]
    fn non_php_files_are_skipped() {
        // Even a repeated "PHP-like" literal in a JS file should not count at
        // this stage — the first-pass detector is PHP-only.
        let mut body = String::from("");
        for _ in 0..30 {
            body.push_str("return ['success' => false, 'error' => $e];\n");
        }
        let file = fp("src/thing.js", &body);
        let findings = detect_repeated_literal_shapes(&[&file]);
        assert!(findings.is_empty());
    }

    #[test]
    fn helper_name_hint_for_error_envelope() {
        let shape: Shape = vec![
            ("success".to_string(), ValueKind::Bool),
            ("error".to_string(), ValueKind::Variable),
            ("message".to_string(), ValueKind::Variable),
        ];
        let name = suggest_helper_name(&shape);
        assert_eq!(name, "error_envelope(...)");
    }

    #[test]
    fn different_shapes_produce_separate_findings() {
        // Shape A (success/error) × 22, Shape B (success/data) × 22 — two distinct findings.
        let mut body = String::from("<?php\n");
        for _ in 0..22 {
            body.push_str("return ['success' => false, 'error' => $e];\n");
        }
        for _ in 0..22 {
            body.push_str("return ['success' => true, 'data' => $d];\n");
        }
        let file = fp("inc/Abilities/Mixed.php", &body);
        let findings = detect_repeated_literal_shapes(&[&file]);
        assert_eq!(findings.len(), 2);
    }
}
