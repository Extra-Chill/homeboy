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

    // Strip trailing body markers — the `{ }` or `{ ... }` are not part of the
    // function's structural contract. Without this, stripping a return type from
    // "register(): void {}" yields "register()" but "register() {}" keeps the
    // braces, causing a false structural mismatch.
    let normalized = strip_body_markers(&normalized);

    // Strip parameter type annotations — only arity and parameter names matter
    // for structural comparison.  This is language-agnostic: for each comma-
    // separated parameter, keep only the last identifier (the parameter name).
    // PHP:  "function execute(array $config)" → "function execute($config)"
    // PHP:  "function handle(WP_REST_Request $request)" → "function handle($request)"
    // Rust: "fn run(args: RunArgs)" → "fn run(args)"
    // Already-untyped params pass through unchanged.

    strip_param_types(&normalized)
}

/// Strip parameter type annotations from a signature string.
///
/// Finds the parameter list (content between `(` and matching `)`) and
/// reduces each comma-separated parameter to just its name — the last
/// identifier-like token. This is language-agnostic:
///
/// - PHP prefix types:  `array $config` → `$config`
/// - PHP class types:   `WP_REST_Request $request` → `$request`
/// - Rust postfix types: `args: RunArgs` → `args`
/// - Rust references:    `config: &Config` → `config`
/// - Variadic/spread:    `...$args` → `...$args` (preserved)
/// - No-type params:     `$request` → `$request` (unchanged)
///
/// Returns the full signature with parameter types stripped.
fn strip_param_types(sig: &str) -> String {
    // Find the parameter list boundaries
    let open = match sig.find('(') {
        Some(pos) => pos,
        None => return sig.to_string(),
    };

    // Find matching close paren (handle nested parens for default values)
    let after_open = &sig[open + 1..];
    let mut depth = 1;
    let mut close_offset = None;
    for (i, ch) in after_open.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_offset = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let close_offset = match close_offset {
        Some(o) => o,
        None => return sig.to_string(),
    };

    let params_str = &after_open[..close_offset];

    // Empty parameter list — nothing to strip
    if params_str.trim().is_empty() {
        return sig.to_string();
    }

    // Split by comma, extract just the parameter name from each
    let normalized_params: Vec<String> = params_str
        .split(',')
        .map(|param| {
            let param = param.trim();
            if param.is_empty() {
                return String::new();
            }
            // The parameter name is the last identifier-like token.
            // Identifiers can contain word chars plus $ (PHP) and & (reference).
            // Walk backward to find it.
            extract_param_name(param)
        })
        .collect();

    let prefix = &sig[..=open];
    let suffix = &sig[open + 1 + close_offset..];
    format!("{}{}{}", prefix, normalized_params.join(", "), suffix)
}

/// Extract just the parameter name from a parameter declaration.
///
/// Handles multiple language patterns generically:
/// - `array $config` → `$config` (PHP prefix type)
/// - `?WP_REST_Request $request` → `$request` (PHP nullable type)
/// - `args: RunArgs` → `args` (Rust postfix type)
/// - `args: &'a RunArgs` → `args` (Rust reference + lifetime)
/// - `$request` → `$request` (no type, PHP)
/// - `self` / `&self` / `&mut self` → `&self` (Rust self param, normalized)
/// - `...int $values` → `...$values` (PHP variadic)
fn extract_param_name(param: &str) -> String {
    let trimmed = param.trim();

    // Rust self parameter — normalize all variants to &self
    if trimmed == "self" || trimmed == "&self" || trimmed == "&mut self" || trimmed == "mut self" {
        return "&self".to_string();
    }

    // Split into tokens on whitespace
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    if tokens.is_empty() {
        return String::new();
    }

    // For Rust postfix type syntax (name: Type), the name is before the colon.
    // Check if any token contains ':' or if ':' appears standalone.
    if let Some(colon_pos) = tokens.iter().position(|t| *t == ":" || t.ends_with(':')) {
        // Everything before the colon is the parameter name (possibly with & or mut)
        if colon_pos > 0 {
            // Take the token just before ':' — that's the name
            let name_token = if tokens[colon_pos].ends_with(':') {
                // Token like "args:" — strip the colon
                &tokens[colon_pos][..tokens[colon_pos].len() - 1]
            } else {
                tokens[colon_pos - 1]
            };
            return name_token.to_string();
        }
    }

    // For PHP prefix type syntax (Type $name) or just ($name),
    // the parameter name is the last token.
    // Also handles variadics: `...Type $name` or `...$name`
    let last = tokens.last().unwrap();

    // If the last token starts with $ or is a bare identifier, that's the name
    // Preserve spread operator if present on the name
    if tokens.len() > 1 {
        // Check if there's a spread operator earlier
        let has_spread = tokens.iter().any(|t| t.starts_with("..."));
        if has_spread && !last.starts_with("...") {
            return format!("...{}", last);
        }
    }

    last.to_string()
}

/// Strip return type declaration from a signature string.
///
/// Finds the last closing paren (end of parameter list) and removes
/// everything after it that looks like a return type annotation.
/// Strip trailing body markers like `{ }`, `{ ... }`, or `{` from a signature.
///
/// Function signatures extracted from source lines often include the opening
/// brace or inline body. These aren't part of the structural contract and
/// should be removed before comparison.
fn strip_body_markers(sig: &str) -> String {
    let trimmed = sig.trim_end();
    // Strip trailing `{ ... }` (inline body)
    if let Some(brace_pos) = trimmed.rfind('{') {
        // Only strip if the `{` comes after the parameter list's `)`
        if let Some(paren_pos) = trimmed.rfind(')') {
            if brace_pos > paren_pos {
                return trimmed[..brace_pos].trim_end().to_string();
            }
        }
    }
    sig.to_string()
}

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

    // --- Parameter type stripping tests ---

    #[test]
    fn php_prefix_type_stripped() {
        let sig = "public function execute(array $config)";
        let normalized = normalize_signature(sig);
        assert!(
            !normalized.contains("array"),
            "PHP type hint should be stripped: {}",
            normalized
        );
        assert!(
            normalized.contains("$config"),
            "Param name preserved: {}",
            normalized
        );
    }

    #[test]
    fn php_class_type_stripped() {
        let sig = "public function handle(WP_REST_Request $request)";
        let normalized = normalize_signature(sig);
        assert!(
            !normalized.contains("WP_REST_Request"),
            "PHP class type should be stripped: {}",
            normalized
        );
        assert!(
            normalized.contains("$request"),
            "Param name preserved: {}",
            normalized
        );
    }

    #[test]
    fn php_typed_and_untyped_same_tokens() {
        let typed = tokenize_signature("public function execute(array $config)");
        let untyped = tokenize_signature("public function execute($config)");
        assert_eq!(
            typed.len(),
            untyped.len(),
            "Token count should match regardless of type hints: {:?} vs {:?}",
            typed,
            untyped
        );
    }

    #[test]
    fn php_class_typed_and_untyped_same_tokens() {
        let typed = tokenize_signature("public function handle(WP_REST_Request $request)");
        let untyped = tokenize_signature("public function handle($request)");
        assert_eq!(
            typed.len(),
            untyped.len(),
            "Token count should match: {:?} vs {:?}",
            typed,
            untyped
        );
    }

    #[test]
    fn php_multiple_params_types_stripped() {
        let typed =
            tokenize_signature("public function execute(array $config, WP_REST_Request $request)");
        let untyped = tokenize_signature("public function execute($config, $request)");
        assert_eq!(
            typed.len(),
            untyped.len(),
            "Token count should match with multiple params: {:?} vs {:?}",
            typed,
            untyped
        );
    }

    #[test]
    fn rust_postfix_type_stripped() {
        let typed = tokenize_signature("pub fn run(args: RunArgs)");
        let untyped = tokenize_signature("pub fn run(args)");
        assert_eq!(
            typed.len(),
            untyped.len(),
            "Rust type annotation should be stripped: {:?} vs {:?}",
            typed,
            untyped
        );
    }

    #[test]
    fn rust_self_param_normalized() {
        let with_self = tokenize_signature("pub fn run(&self, args: RunArgs)");
        let with_mut_self = tokenize_signature("pub fn run(&mut self, args: RunArgs)");
        assert_eq!(
            with_self.len(),
            with_mut_self.len(),
            "&self and &mut self should normalize the same: {:?} vs {:?}",
            with_self,
            with_mut_self
        );
    }

    #[test]
    fn empty_params_unchanged() {
        let sig = "public function register()";
        let normalized = normalize_signature(sig);
        assert!(
            normalized.contains("()"),
            "Empty params should stay empty: {}",
            normalized
        );
    }

    #[test]
    fn php_nullable_type_stripped() {
        let typed = tokenize_signature("public function get(?string $name)");
        let untyped = tokenize_signature("public function get($name)");
        assert_eq!(
            typed.len(),
            untyped.len(),
            "Nullable type should be stripped: {:?} vs {:?}",
            typed,
            untyped
        );
    }

    #[test]
    fn skeleton_matches_with_type_differences() {
        let sigs = vec![
            tokenize_signature("public function execute(array $config)"),
            tokenize_signature("public function execute($config)"),
        ];
        let skeleton = compute_signature_skeleton(&sigs);
        assert!(
            skeleton.is_some(),
            "Skeleton should compute despite type hint differences"
        );
    }
}
