//! Body hashing and structural normalization for grammar fingerprints.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::core::extension::grammar::Grammar;

/// Compute exact body hash: normalize whitespace, SHA256, truncate to 16 hex chars.
pub(super) fn exact_hash(body: &str) -> String {
    let normalized = normalize_whitespace(body);
    sha256_hex16(&normalized)
}

/// Compute structural hash: replace identifiers/literals with positional tokens.
pub(super) fn structural_hash(body: &str, grammar: &Grammar) -> String {
    let normalized = structural_normalize(body, grammar);
    sha256_hex16(&normalized)
}

/// Normalize whitespace: collapse all runs to single space.
pub(super) fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(ch);
            in_space = false;
        }
    }
    result.trim().to_string()
}

/// SHA256 hash, return first 16 hex characters.
fn sha256_hex16(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    format!("{:x}", hash)[..16].to_string()
}

/// Structural normalization: strip to body, replace strings/numbers/identifiers
/// with positional tokens, preserving language keywords as structural markers.
fn structural_normalize(body: &str, grammar: &Grammar) -> String {
    // Strip to body (from first opening brace)
    let text = if let Some(pos) = body.find('{') {
        &body[pos..]
    } else {
        body
    };

    let keyword_set: HashSet<&str> = grammar
        .fingerprint
        .keywords
        .iter()
        .map(|keyword| keyword.as_str())
        .collect();

    // Working string — we'll do sequential replacements
    let mut result = text.to_string();

    // Replace string literals with STR
    result = replace_string_literals(&result);

    // Replace numeric literals with NUM
    result = replace_numeric_literals(&result);

    let preserved_variables = effective_preserved_variables(grammar);
    for prefix in effective_variable_prefixes(grammar) {
        result = replace_prefixed_variables(&result, &prefix, &preserved_variables);
    }

    // Replace non-keyword identifiers with positional tokens
    result = replace_identifiers(&result, &keyword_set);

    // Collapse whitespace
    normalize_whitespace(&result)
}

/// Replace string literals ("..." and '...') with STR.
pub(super) fn replace_string_literals(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            i += 1;
            // Skip contents until matching unescaped quote
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2; // skip escaped char
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            result.push_str("STR");
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Replace numeric literals with NUM.
fn replace_numeric_literals(input: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b\d[\d_]*(?:\.\d[\d_]*)?\b").unwrap());
    RE.replace_all(input, "NUM").to_string()
}

fn effective_variable_prefixes(grammar: &Grammar) -> Vec<String> {
    if !grammar.fingerprint.variable_prefixes.is_empty() {
        return grammar.fingerprint.variable_prefixes.clone();
    }

    // Compatibility for existing external grammars: infer dollar-prefixed
    // variables from grammar-owned patterns rather than language names.
    let has_dollar_pattern = grammar
        .patterns
        .values()
        .any(|pattern| pattern.regex.contains("\\$") || pattern.regex.contains('$'));

    if has_dollar_pattern {
        vec!["$".to_string()]
    } else {
        Vec::new()
    }
}

fn effective_preserved_variables(grammar: &Grammar) -> HashSet<String> {
    let mut preserved: HashSet<String> = grammar
        .fingerprint
        .preserved_variables
        .iter()
        .cloned()
        .collect();

    // Compatibility for existing dollar-prefixed grammars that relied on the
    // previous hardcoded structural treatment for object receiver references.
    if preserved.is_empty()
        && effective_variable_prefixes(grammar)
            .iter()
            .any(|p| p == "$")
    {
        preserved.insert("$this".to_string());
    }

    preserved
}

/// Replace prefixed variable references with positional tokens.
fn replace_prefixed_variables(input: &str, prefix: &str, preserved: &HashSet<String>) -> String {
    let Ok(re) = regex::Regex::new(&format!(r"{}\w+", regex::escape(prefix))) else {
        return input.to_string();
    };
    let mut var_map: HashMap<String, String> = HashMap::new();
    let mut counter = 0;

    re.replace_all(input, |caps: &regex::Captures| {
        let var = caps[0].to_string();
        if preserved.contains(&var) {
            return var;
        }
        let token = var_map.entry(var).or_insert_with(|| {
            let t = format!("VAR_{}", counter);
            counter += 1;
            t
        });
        token.clone()
    })
    .to_string()
}

/// Replace non-keyword identifiers with positional ID_N tokens.
fn replace_identifiers(input: &str, keywords: &HashSet<&str>) -> String {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b[a-zA-Z_]\w*\b").unwrap());
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut counter = 0;

    RE.replace_all(input, |caps: &regex::Captures| {
        let word = &caps[0];
        if keywords.contains(word) {
            return word.to_string();
        }
        // Also preserve structural tokens we inserted
        if word.starts_with("STR")
            || word.starts_with("NUM")
            || word.starts_with("CHR")
            || word.starts_with("VAR_")
            || word.starts_with("ID_")
        {
            return word.to_string();
        }
        let token = id_map.entry(word.to_string()).or_insert_with(|| {
            let t = format!("ID_{}", counter);
            counter += 1;
            t
        });
        token.clone()
    })
    .to_string()
}
