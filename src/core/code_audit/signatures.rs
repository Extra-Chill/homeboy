//! signatures — extracted from conventions.rs.

use regex::Regex;

/// Normalize a signature string before tokenization.
///
/// Collapses whitespace/newlines, removes trailing commas before closing
/// parens, normalizes extension path references, and strips return type
/// declarations. This is language-agnostic — works on any signature string.
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

    // Strip return type declarations — they don't change the calling convention
    // and shouldn't cause structural mismatches.
    // PHP:  "function foo(): void" → "function foo()"
    // PHP:  "function foo(): ?array" → "function foo()"
    // Rust: "fn foo() -> Result<T>" → "fn foo()"
    let normalized = strip_return_type(&normalized);

    normalized
}

/// Strip return type declaration from a signature string.
///
/// Finds the last closing paren (end of parameter list) and removes
/// everything after it that looks like a return type annotation.
fn strip_return_type(sig: &str) -> String {
    // Find the last ')' — that's the end of the parameter list
    if let Some(paren_pos) = sig.rfind(')') {
        let after_paren = &sig[paren_pos + 1..].trim_start();
        // PHP return type: ": void", ": ?array", ": \Namespace\Type"
        // Rust return type: "-> Result<T>", "-> bool"
        if after_paren.starts_with(':') || after_paren.starts_with("->") {
            return sig[..=paren_pos].to_string();
        }
    }
    sig.to_string()
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
pub(crate) fn compute_signature_skeleton(
    tokenized_sigs: &[Vec<String>],
) -> Option<Vec<Option<String>>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_return_type_php_void() {
        let sig = "public function register(): void";
        let normalized = normalize_signature(sig);
        assert!(
            !normalized.contains("void"),
            "Return type should be stripped: {}",
            normalized
        );
    }

    #[test]
    fn strip_return_type_php_nullable() {
        let sig = "public function get_items(): ?array";
        let normalized = normalize_signature(sig);
        assert!(
            !normalized.contains("array"),
            "Return type should be stripped: {}",
            normalized
        );
    }

    #[test]
    fn strip_return_type_rust() {
        let sig = "pub fn run(args: FooArgs) -> CmdResult<FooOutput>";
        let normalized = normalize_signature(sig);
        assert!(
            !normalized.contains("CmdResult"),
            "Return type should be stripped: {}",
            normalized
        );
    }

    #[test]
    fn strip_return_type_preserves_params() {
        let sig = "pub fn run(args: FooArgs)";
        let normalized = normalize_signature(sig);
        assert!(
            normalized.contains("FooArgs"),
            "Params should be preserved: {}",
            normalized
        );
    }

    #[test]
    fn same_tokens_with_and_without_return_type() {
        let with_return = tokenize_signature("public function register(): void");
        let without_return = tokenize_signature("public function register()");
        assert_eq!(
            with_return.len(),
            without_return.len(),
            "Token count should match regardless of return type: {:?} vs {:?}",
            with_return,
            without_return
        );
    }

    #[test]
    fn rust_return_type_stripped_in_skeleton() {
        let sigs = vec![
            tokenize_signature("pub fn run(args: RunArgs) -> Result<Output>"),
            tokenize_signature("pub fn run(args: RunArgs)"),
        ];
        let skeleton = compute_signature_skeleton(&sigs);
        assert!(
            skeleton.is_some(),
            "Skeleton should compute successfully after return type stripping"
        );
    }

    #[test]
    fn tokenize_preserves_function_name() {
        let tokens = tokenize_signature("pub fn do_stuff(x: i32)");
        assert!(tokens.contains(&"do_stuff".to_string()));
    }
}
