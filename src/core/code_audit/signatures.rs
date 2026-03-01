//! signatures — extracted from conventions.rs.

use regex::Regex;

/// Normalize a signature string before tokenization.
///
/// Collapses whitespace/newlines, removes trailing commas before closing
/// parens, and normalizes extension path references to just the final segment.
/// This is language-agnostic — works on any signature string.
pub(crate) fn normalize_signature(sig: &str) -> String {
    // Collapse all whitespace (including newlines) into single spaces
    let normalized: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");

    // Remove trailing comma before closing paren: ", )" → ")"
    let normalized = Regex::new(r",\s*\)")
        .unwrap()
        .replace_all(&normalized, ")")
        .to_string();

    // Normalize extension paths to final segment: crate::commands::GlobalArgs → GlobalArgs
    // Also handles super::GlobalArgs → GlobalArgs
    // This is generic: any sequence of word::word::...::Word keeps only the last part
    let normalized = Regex::new(r"\b(?:\w+::)+(\w+)")
        .unwrap()
        .replace_all(&normalized, "$1")
        .to_string();

    // Strip parameter modifiers that don't affect the structural contract.
    // "mut" before a parameter name is a local annotation, not part of the
    // function's external signature. E.g., "fn run(mut args: T)" → "fn run(args: T)"
    let normalized = Regex::new(r"\bmut\s+")
        .unwrap()
        .replace_all(&normalized, "")
        .to_string();

    normalized
}

/// Split a signature string into tokens for structural comparison.
///
/// Splits on whitespace and punctuation boundaries while preserving the
/// punctuation as separate tokens. This is language-agnostic — it works
/// on any signature string regardless of language.
///
/// Example: `pub fn run(args: FooArgs, _global: &GlobalArgs) -> CmdResult<FooOutput>`
/// becomes: `["pub", "fn", "run", "(", "args", ":", "FooArgs", ",", "_global", ":", "&", "GlobalArgs", ")", "->", "CmdResult", "<", "FooOutput", ">"]`
pub(crate) fn tokenize_signature(sig: &str) -> Vec<String> {
    let sig = normalize_signature(sig);
    let mut tokens = Vec::new();
    let mut current = String::new();

    for ch in sig.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            // Punctuation: flush current word, then emit punctuation as token
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            // Group -> as a single token
            if ch == '-' {
                current.push(ch);
            } else if ch == '>' && current == "-" {
                current.push(ch);
                tokens.push(std::mem::take(&mut current));
            } else {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                tokens.push(ch.to_string());
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Compute the structural skeleton of a set of signatures for the same method.
///
/// Given multiple tokenized signatures, identifies which token positions are
/// constant (same across all signatures) vs. variable (differ per file).
/// Returns the skeleton as a vec of `Some(token)` for constant positions
/// and `None` for variable positions, plus the expected token count.
///
/// If signatures have different token counts (different arity/structure),
/// returns `None` — those are real structural mismatches.
pub(crate) fn compute_signature_skeleton(tokenized_sigs: &[Vec<String>]) -> Option<Vec<Option<String>>> {
    if tokenized_sigs.is_empty() {
        return None;
    }

    let expected_len = tokenized_sigs[0].len();

    // All signatures must have the same number of tokens
    if !tokenized_sigs.iter().all(|t| t.len() == expected_len) {
        // Different token counts = structural mismatch, can't build skeleton
        return None;
    }

    let mut skeleton = Vec::with_capacity(expected_len);
    for i in 0..expected_len {
        let first = &tokenized_sigs[0][i];
        if tokenized_sigs.iter().all(|t| &t[i] == first) {
            skeleton.push(Some(first.clone()));
        } else {
            skeleton.push(None); // This position varies — it's a "type parameter"
        }
    }

    Some(skeleton)
}
