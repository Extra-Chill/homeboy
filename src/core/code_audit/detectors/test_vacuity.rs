//! Generic vacuity detection for brace-delimited test methods.
//!
//! "Vacuous" tests are no-op or placeholder tests that do not exercise product
//! code — empty bodies, `assert!(true)`-style placeholders, or bodies whose
//! only intent is audit bookkeeping. The detector is language-agnostic: it
//! operates on brace-delimited (`{ ... }`) function bodies and is driven by a
//! `TestVacuityPolicy` declared by an extension manifest, which supplies the
//! applicable file extensions, the markers that prove a test references product
//! code, deliberate-contract markers, and an optional package-name resolver.

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use crate::core::extension::{PackageNameSource, TestVacuityPolicy};

/// Resolve a package name from a config-declared manifest source.
///
/// This is language-agnostic: the caller declares the manifest filename, an
/// optional section header, and the key whose value names the package. The
/// resolved name lets vacuity detection treat `<package>::` references as
/// product references without core knowing the ecosystem.
pub(crate) fn resolve_package_name(root: &Path, source: &PackageNameSource) -> Option<String> {
    let manifest = std::fs::read_to_string(root.join(&source.manifest_file)).ok()?;
    let scoped = match &source.section {
        Some(section) => {
            let start = manifest.find(section.as_str())?;
            let rest = &manifest[start..];
            // Stop at the next section header on its own line, if any.
            let end = rest[section.len()..]
                .find("\n[")
                .map(|idx| section.len() + idx + 1)
                .unwrap_or(rest.len());
            &rest[..end]
        }
        None => manifest.as_str(),
    };

    for line in scoped.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(&source.key) {
            let value = value.trim_start();
            if let Some(value) = value.strip_prefix('=') {
                let name = value.trim().trim_matches('"').trim_matches('\'');
                if name.is_empty() {
                    continue;
                }
                return Some(if source.normalize_dashes_to_underscores {
                    name.replace('-', "_")
                } else {
                    name.to_string()
                });
            }
        }
    }
    None
}

/// Detect vacuous test methods in a fingerprint according to a vacuity policy.
///
/// `package_name` is the resolved package name (see [`resolve_package_name`]),
/// passed in by the caller so it is resolved once per component.
pub(crate) fn find_vacuous_test_methods(
    findings: &mut Vec<Finding>,
    fp: &FileFingerprint,
    test_methods: &[String],
    source_methods: &HashSet<&str>,
    policy: &TestVacuityPolicy,
    package_name: Option<&str>,
) {
    if fp.content.trim().is_empty() || !policy_applies_to(policy, &fp.relative_path) {
        return;
    }

    let product_symbols = collect_product_imports(&fp.content, package_name);
    for method in test_methods {
        let Some(body) = extract_function_body(&fp.content, method) else {
            continue;
        };
        let Some(reason) = classify_vacuous_test(
            &body,
            &product_symbols,
            source_methods,
            policy,
            package_name,
        ) else {
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

fn policy_applies_to(policy: &TestVacuityPolicy, relative_path: &str) -> bool {
    let ext = Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    policy
        .file_extensions
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

fn classify_vacuous_test(
    body: &str,
    product_symbols: &HashSet<String>,
    source_methods: &HashSet<&str>,
    policy: &TestVacuityPolicy,
    package_name: Option<&str>,
) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    if policy.allowed_body_markers.iter().any(|marker| {
        lower.contains(&marker.to_ascii_lowercase()) || body.contains(marker.as_str())
    }) {
        return None;
    }

    let uncommented = strip_comments(body);
    let compact: String = uncommented.chars().filter(|c| !c.is_whitespace()).collect();
    let has_assertion = uncommented.contains("assert") || uncommented.contains("panic!");
    let has_product_ref = has_product_reference(
        &uncommented,
        product_symbols,
        source_methods,
        policy,
        package_name,
    );

    if has_product_ref {
        return None;
    }

    if compact == "assert!(true);" || compact == "assert!(true)" {
        return Some("it only asserts true".to_string());
    }
    if compact.contains("assert!(true)") {
        return Some("it contains only placeholder assertion logic".to_string());
    }
    let comment_text = collect_comments(body).to_ascii_lowercase();
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

fn has_product_reference(
    body: &str,
    product_symbols: &HashSet<String>,
    source_methods: &HashSet<&str>,
    policy: &TestVacuityPolicy,
    package_name: Option<&str>,
) -> bool {
    if policy
        .product_reference_markers
        .iter()
        .any(|marker| body.contains(marker.as_str()))
    {
        return true;
    }
    if let Some(name) = package_name {
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

/// Collect product symbols imported from the resolved package via `use` lines.
///
/// This recognizes the common `use <package>::path::Symbol;` and
/// `use <package>::path::{A, B};` import shapes shared by several languages.
/// When no package name is resolved, no symbols are collected.
fn collect_product_imports(content: &str, package_name: Option<&str>) -> HashSet<String> {
    let mut symbols = HashSet::new();
    let Some(package_name) = package_name else {
        return symbols;
    };
    let simple = Regex::new(&format!(
        r"(?m)^\s*use\s+{}::[^;:]+::([A-Za-z_][A-Za-z0-9_]*)\s*;",
        regex::escape(package_name)
    ))
    .unwrap();
    for cap in simple.captures_iter(content) {
        symbols.insert(cap[1].to_string());
    }

    let grouped = Regex::new(&format!(
        r"(?m)^\s*use\s+{}::[^;]*\{{([^}}]+)\}}\s*;",
        regex::escape(package_name)
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

/// Extract the brace-delimited body of `fn_name` from brace-using source.
///
/// Matches a `fn <name>(...)` declaration followed by `{ ... }` and returns the
/// inner text. Brace-delimited function syntax is shared across many languages,
/// so this is a generic block-body extractor rather than a language-specific
/// one.
fn extract_function_body(content: &str, fn_name: &str) -> Option<String> {
    let pattern = Regex::new(&format!(
        r"(?m)\bfn\s+{}\s*\([^)]*\)\s*(?:->[^{{]+)?\{{",
        regex::escape(fn_name)
    ))
    .ok()?;
    let mat = pattern.find(content)?;
    let open = mat.end() - 1;
    matching_brace(content, open).map(|end| content[mat.end()..end].to_string())
}

fn matching_brace(content: &str, open: usize) -> Option<usize> {
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

fn strip_comments(content: &str) -> String {
    let without_blocks = Regex::new(r"(?s)/\*.*?\*/")
        .unwrap()
        .replace_all(content, "");
    without_blocks
        .lines()
        .map(|line| line.split_once("//").map(|(code, _)| code).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_comments(content: &str) -> String {
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

    fn policy() -> TestVacuityPolicy {
        TestVacuityPolicy {
            file_extensions: vec!["rs".to_string()],
            allowed_body_markers: vec![
                "compile contract".to_string(),
                "compile-only".to_string(),
                "assert_snapshot".to_string(),
                "assert_debug_snapshot".to_string(),
            ],
            product_reference_markers: vec![
                "crate::".to_string(),
                "super::".to_string(),
                "self::".to_string(),
            ],
            package_name: Some(PackageNameSource {
                manifest_file: "Cargo.toml".to_string(),
                section: Some("[package]".to_string()),
                key: "name".to_string(),
                normalize_dashes_to_underscores: true,
            }),
        }
    }

    #[test]
    fn extract_function_body_with_unbalanced_braces_inside_raw_string() {
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

        let body = extract_function_body(content, "build_grammar").expect("body");

        assert!(body.contains("Grammar"));
        assert!(body.contains("regex"));
    }

    #[test]
    fn extract_function_body_with_hash_raw_string_containing_braces() {
        let content = r##"
fn parse_json() {
    let value = r#"{"name":"package"}"#;
    assert!(value.contains("package"));
}
"##;

        let body = extract_function_body(content, "parse_json").expect("body");

        assert!(body.contains("assert!"));
    }

    #[test]
    fn classify_vacuous_test_ignores_code_and_string_literals() {
        let body = r#"
            let finding = Finding {
                convention: "test_coverage".to_string(),
                kind: AuditFinding::MissingTestFile,
            };
            assert_eq!(finding.convention, "test_coverage");
        "#;

        assert_eq!(
            classify_vacuous_test(body, &HashSet::new(), &HashSet::new(), &policy(), None),
            None
        );
    }

    #[test]
    fn classify_vacuous_test_flags_comment_only_mapping_tests() {
        let body = r#"
            // Keep this audit coverage mapping test wired.
            assert_eq!(1, 1);
        "#;

        assert_eq!(
            classify_vacuous_test(body, &HashSet::new(), &HashSet::new(), &policy(), None),
            Some("its comments describe audit coverage mapping instead of behavior".to_string())
        );
    }

    #[test]
    fn classify_vacuous_test_flags_assert_true_placeholder() {
        let body = "assert!(true);";
        assert_eq!(
            classify_vacuous_test(body, &HashSet::new(), &HashSet::new(), &policy(), None),
            Some("it only asserts true".to_string())
        );
    }

    #[test]
    fn classify_vacuous_test_accepts_product_reference_marker() {
        let body = "let result = crate::run();\nassert!(result);";
        assert_eq!(
            classify_vacuous_test(body, &HashSet::new(), &HashSet::new(), &policy(), None),
            None
        );
    }

    #[test]
    fn resolve_package_name_reads_manifest_section_key() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-tool\"\nversion = \"1.0.0\"\n",
        )
        .expect("write manifest");

        let resolved = resolve_package_name(
            dir.path(),
            &PackageNameSource {
                manifest_file: "Cargo.toml".to_string(),
                section: Some("[package]".to_string()),
                key: "name".to_string(),
                normalize_dashes_to_underscores: true,
            },
        );

        assert_eq!(resolved.as_deref(), Some("my_tool"));
    }
}
