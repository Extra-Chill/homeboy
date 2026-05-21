use std::collections::HashSet;
use std::path::Path;

use regex::Regex;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

pub(crate) fn rust_crate_name(root: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let package_start = manifest.find("[package]")?;
    let package = &manifest[package_start..];
    for line in package.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("name") {
            let value = value.trim_start();
            if let Some(value) = value.strip_prefix('=') {
                return Some(value.trim().trim_matches('"').replace('-', "_"));
            }
        }
    }
    None
}

pub(crate) fn find_vacuous_test_methods(
    findings: &mut Vec<Finding>,
    fp: &FileFingerprint,
    test_methods: &[String],
    source_methods: &HashSet<&str>,
    crate_name: Option<&str>,
) {
    if fp.language != Language::Rust || fp.content.trim().is_empty() {
        return;
    }

    let product_symbols = collect_rust_product_imports(&fp.content, crate_name);
    for method in test_methods {
        let Some(body) = extract_rust_function_body(&fp.content, method) else {
            continue;
        };
        let Some(reason) =
            classify_vacuous_rust_test(&body, &product_symbols, source_methods, crate_name)
        else {
            continue;
        };

        findings.push(Finding {
            convention: "test_coverage".to_string(),
            severity: Severity::Info,
            file: fp.relative_path.clone(),
            description: format!("Test method '{}' is vacuous: {}", method, reason),
            suggestion: format!(
                "Remove '{}' or replace it with a behavior test that exercises product code",
                method
            ),
            kind: AuditFinding::VacuousTest,
        });
    }
}

fn classify_vacuous_rust_test(
    body: &str,
    product_symbols: &HashSet<String>,
    source_methods: &HashSet<&str>,
    crate_name: Option<&str>,
) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    if lower.contains("compile contract") || lower.contains("compile-only") {
        return None;
    }
    if body.contains("assert_snapshot") || body.contains("assert_debug_snapshot") {
        return None;
    }

    let uncommented = strip_rust_comments(body);
    let compact: String = uncommented.chars().filter(|c| !c.is_whitespace()).collect();
    let has_assertion = uncommented.contains("assert") || uncommented.contains("panic!");
    let has_product_ref =
        has_rust_product_reference(&uncommented, product_symbols, source_methods, crate_name);

    if has_product_ref {
        return None;
    }

    if compact == "assert!(true);" || compact == "assert!(true)" {
        return Some("it only asserts true".to_string());
    }
    if compact.contains("assert!(true)") {
        return Some("it contains only placeholder assertion logic".to_string());
    }
    let comment_text = collect_rust_comments(body).to_ascii_lowercase();
    if comment_text.contains("audit")
        && (comment_text.contains("mapping") || comment_text.contains("coverage"))
    {
        return Some(
            "its comments describe audit coverage mapping instead of behavior".to_string(),
        );
    }
    if !has_assertion && compact.is_empty() {
        return Some("it has an empty body".to_string());
    }
    None
}

fn has_rust_product_reference(
    body: &str,
    product_symbols: &HashSet<String>,
    source_methods: &HashSet<&str>,
    crate_name: Option<&str>,
) -> bool {
    if body.contains("crate::") || body.contains("super::") || body.contains("self::") {
        return true;
    }
    if let Some(name) = crate_name {
        if body.contains(&format!("{}::", name)) {
            return true;
        }
    }
    product_symbols
        .iter()
        .any(|symbol| contains_word_call(body, symbol) || body.contains(&format!("{}::", symbol)))
        || source_methods
            .iter()
            .any(|method| contains_word_call(body, method))
}

fn contains_word_call(haystack: &str, needle: &str) -> bool {
    let pattern = format!(r"\b{}\s*\(", regex::escape(needle));
    Regex::new(&pattern)
        .ok()
        .is_some_and(|re| re.is_match(haystack))
}

fn collect_rust_product_imports(content: &str, crate_name: Option<&str>) -> HashSet<String> {
    let mut symbols = HashSet::new();
    let Some(crate_name) = crate_name else {
        return symbols;
    };
    let simple = Regex::new(&format!(
        r"(?m)^\s*use\s+{}::[^;:]+::([A-Za-z_][A-Za-z0-9_]*)\s*;",
        regex::escape(crate_name)
    ))
    .unwrap();
    for cap in simple.captures_iter(content) {
        symbols.insert(cap[1].to_string());
    }

    let grouped = Regex::new(&format!(
        r"(?m)^\s*use\s+{}::[^;]*\{{([^}}]+)\}}\s*;",
        regex::escape(crate_name)
    ))
    .unwrap();
    for cap in grouped.captures_iter(content) {
        for raw in cap[1].split(',') {
            let symbol = raw.trim().trim_start_matches("self::");
            let symbol = symbol.split_whitespace().next().unwrap_or("");
            if !symbol.is_empty()
                && symbol
                    .chars()
                    .all(|c| c == '_' || c.is_ascii_alphanumeric())
            {
                symbols.insert(symbol.to_string());
            }
        }
    }

    symbols
}

fn extract_rust_function_body(content: &str, fn_name: &str) -> Option<String> {
    let pattern = Regex::new(&format!(
        r"(?m)\bfn\s+{}\s*\([^)]*\)\s*(?:->[^{{]+)?\{{",
        regex::escape(fn_name)
    ))
    .ok()?;
    let mat = pattern.find(content)?;
    let open = mat.end() - 1;
    matching_rust_brace(content, open).map(|end| content[mat.end()..end].to_string())
}

fn matching_rust_brace(content: &str, open: usize) -> Option<usize> {
    if content.as_bytes().get(open) != Some(&b'{') {
        return None;
    }

    let mut depth = 0_i32;
    let mut iter = content[open..].char_indices().peekable();
    while let Some((idx, ch)) = iter.next() {
        let absolute = open + idx;
        match ch {
            '/' if iter.peek().is_some_and(|(_, next)| *next == '/') => {
                skip_line_comment(&mut iter);
            }
            '/' if iter.peek().is_some_and(|(_, next)| *next == '*') => {
                iter.next();
                skip_block_comment(&mut iter);
            }
            'r' if raw_string_hashes(content, absolute).is_some() => {
                let hashes = raw_string_hashes(content, absolute)?;
                skip_raw_string(&mut iter, hashes);
            }
            '"' => skip_quoted_string(&mut iter),
            '\'' => skip_char_literal(&mut iter),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(absolute);
                }
            }
            _ => {}
        }
    }
    None
}

fn skip_line_comment(iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    for (_, ch) in iter.by_ref() {
        if ch == '\n' {
            break;
        }
    }
}

fn skip_block_comment(iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    let mut previous = '\0';
    for (_, ch) in iter.by_ref() {
        if previous == '*' && ch == '/' {
            break;
        }
        previous = ch;
    }
}

fn raw_string_hashes(content: &str, offset: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes.get(offset) != Some(&b'r') {
        return None;
    }
    let mut idx = offset + 1;
    let mut hashes = 0;
    while bytes.get(idx) == Some(&b'#') {
        hashes += 1;
        idx += 1;
    }
    (bytes.get(idx) == Some(&b'"')).then_some(hashes)
}

fn skip_raw_string(iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>, hashes: usize) {
    let mut saw_opening_quote = false;
    for (_, ch) in iter.by_ref() {
        if ch == '"' {
            saw_opening_quote = true;
            break;
        }
    }
    if !saw_opening_quote {
        return;
    }

    while let Some((_, ch)) = iter.next() {
        if ch != '"' {
            continue;
        }
        if hashes == 0 {
            break;
        }

        let mut hash_count = 0usize;
        while iter.peek().is_some_and(|(_, next)| *next == '#') {
            iter.next();
            hash_count += 1;
            if hash_count == hashes {
                break;
            }
        }
        if hash_count == hashes {
            break;
        }
    }
}

fn skip_quoted_string(iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    let mut escaped = false;
    for (_, ch) in iter.by_ref() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            break;
        }
    }
}

fn skip_char_literal(iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    let mut escaped = false;
    for (_, ch) in iter.by_ref() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '\'' {
            break;
        }
    }
}

fn strip_rust_comments(content: &str) -> String {
    let without_blocks = Regex::new(r"(?s)/\*.*?\*/")
        .unwrap()
        .replace_all(content, "");
    without_blocks
        .lines()
        .map(|line| line.split_once("//").map(|(code, _)| code).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_rust_comments(content: &str) -> String {
    let mut comments = String::new();
    let mut iter = content.char_indices().peekable();
    while let Some((idx, ch)) = iter.next() {
        match ch {
            '/' if iter.peek().is_some_and(|(_, next)| *next == '/') => {
                iter.next();
                collect_line_comment(&mut iter, &mut comments);
            }
            '/' if iter.peek().is_some_and(|(_, next)| *next == '*') => {
                iter.next();
                collect_block_comment(&mut iter, &mut comments);
            }
            'r' if raw_string_hashes(content, idx).is_some() => {
                if let Some(hashes) = raw_string_hashes(content, idx) {
                    skip_raw_string(&mut iter, hashes);
                }
            }
            '"' => skip_quoted_string(&mut iter),
            '\'' => skip_char_literal(&mut iter),
            _ => {}
        }
    }
    comments
}

fn collect_line_comment(
    iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    comments: &mut String,
) {
    for (_, ch) in iter.by_ref() {
        if ch == '\n' {
            comments.push('\n');
            break;
        }
        comments.push(ch);
    }
}

fn collect_block_comment(
    iter: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    comments: &mut String,
) {
    let mut previous = '\0';
    for (_, ch) in iter.by_ref() {
        if previous == '*' && ch == '/' {
            comments.pop();
            comments.push('\n');
            break;
        }
        comments.push(ch);
        previous = ch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_body_with_unbalanced_braces_inside_raw_string() {
        let content = r#"
#[cfg(test)]
mod tests {
    fn build_grammar() -> Grammar {
        Grammar {
            regex: r"(?:::\{[^}]+\})?".to_string(),
        }
    }
}
"#;

        let body = extract_rust_function_body(content, "build_grammar").expect("body");

        assert!(body.contains("Grammar"));
        assert!(body.contains("regex"));
    }

    #[test]
    fn extracts_body_with_hash_raw_string_containing_braces() {
        let content = r##"
fn parse_json() {
    let value = r#"{"name":"homeboy"}"#;
    assert!(value.contains("homeboy"));
}
"##;

        let body = extract_rust_function_body(content, "parse_json").expect("body");

        assert!(body.contains("assert!"));
    }

    #[test]
    fn vacuity_mapping_comment_heuristic_ignores_code_and_string_literals() {
        let body = r#"
            let finding = Finding {
                convention: "test_coverage".to_string(),
                kind: AuditFinding::MissingTestFile,
            };
            assert_eq!(finding.convention, "test_coverage");
        "#;

        assert_eq!(
            classify_vacuous_rust_test(body, &HashSet::new(), &HashSet::new(), None),
            None
        );
    }

    #[test]
    fn vacuity_mapping_comment_heuristic_flags_comment_only_mapping_tests() {
        let body = r#"
            // Keep this audit coverage mapping test wired.
            assert_eq!(1, 1);
        "#;

        assert_eq!(
            classify_vacuous_rust_test(body, &HashSet::new(), &HashSet::new(), None),
            Some("its comments describe audit coverage mapping instead of behavior".to_string())
        );
    }
}
