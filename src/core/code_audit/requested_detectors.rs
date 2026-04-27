//! Targeted detector rules requested from real audit misses.
//!
//! These are intentionally conservative text-backed rules. They catch common
//! WordPress/PHP drift shapes without pretending to be a full PHP data-flow
//! engine: JSON blob exact matching via SQL LIKE, raw slug literals when a
//! matching class constant exists, and doc/implementation drift around network
//! option storage.

use std::sync::LazyLock;

use regex::Regex;

use super::comment_blocks;
use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

#[derive(Debug, Clone)]
struct SlugConstant {
    class_name: Option<String>,
    const_name: String,
    value: String,
    file: String,
}

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let php_files: Vec<&FileFingerprint> = fingerprints
        .iter()
        .copied()
        .filter(|fp| fp.language == Language::Php && !is_vendored_path(&fp.relative_path))
        .collect();

    let mut findings = Vec::new();
    findings.extend(detect_json_like_exact_matches(&php_files));
    findings.extend(detect_constant_backed_slug_literals(&php_files));
    findings.extend(detect_option_scope_drift(&php_files));
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn is_vendored_path(path: &str) -> bool {
    path.contains("/vendor/")
        || path.starts_with("vendor/")
        || path.contains("/node_modules/")
        || path.starts_with("node_modules/")
}

// ============================================================================
// #1559 — SQL LIKE exact matches against JSON blob fields
// ============================================================================

static JSON_LIKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)\b(metadata|engine_data|config|payload)\b\s+LIKE\s+[^;\n]*(?:\\?['\"]|%)(?:\\?\")([A-Za-z_][A-Za-z0-9_\-]*)(?:\\?\")\s*:"#,
    )
    .expect("JSON LIKE detector regex compiles")
});

fn detect_json_like_exact_matches(files: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for fp in files {
        for cap in JSON_LIKE_RE.captures_iter(&fp.content) {
            let full = cap.get(0).unwrap();
            let column = cap.get(1).map(|m| m.as_str()).unwrap_or("JSON blob");
            let field = cap.get(2).map(|m| m.as_str()).unwrap_or("field");
            let line = line_of_offset(&fp.content, full.start());
            findings.push(Finding {
                convention: "requested_detectors".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "SQL LIKE exact-match against JSON blob `{}` at line {} for key `{}`",
                    column, line, field
                ),
                suggestion: format!(
                    "Avoid semantic matching inside `{}` with LIKE. Decode candidate rows or promote `{}` to a first-class indexed column before exact matching.",
                    column, field
                ),
                kind: AuditFinding::JsonLikeExactMatch,
            });
        }
    }
    findings
}

// ============================================================================
// #1560 — Literal slug drift when matching constants exist
// ============================================================================

static CLASS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(?:final\s+|abstract\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)"#)
        .expect("class regex compiles")
});

static CONST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)\b(?:(?:public|protected|private)\s+)?const\s+([A-Z][A-Z0-9_]*)\s*=\s*['\"]([^'\"]+)['\"]"#,
    )
    .expect("constant regex compiles")
});

static SLUG_VALUE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^[a-z][a-z0-9]*(?:[-_/:][a-z0-9]+)+$"#).expect("slug regex compiles")
});

fn detect_constant_backed_slug_literals(files: &[&FileFingerprint]) -> Vec<Finding> {
    let constants = collect_slug_constants(files);
    if constants.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for constant in constants {
        let literal_re = Regex::new(&format!(r#"['\"]{}['\"]"#, regex::escape(&constant.value)))
            .expect("escaped literal regex compiles");
        for fp in files {
            if fp.relative_path == constant.file {
                continue;
            }
            let Some(m) = literal_re.find(&fp.content) else {
                continue;
            };
            let line = line_of_offset(&fp.content, m.start());
            findings.push(Finding {
                convention: "requested_detectors".to_string(),
                severity: Severity::Info,
                file: fp.relative_path.clone(),
                description: format!(
                    "Raw slug literal `{}` at line {} duplicates constant {}",
                    constant.value,
                    line,
                    constant_label(&constant)
                ),
                suggestion: format!(
                    "Use {} instead of repeating the literal slug so the constant remains the source of truth.",
                    constant_label(&constant)
                ),
                kind: AuditFinding::ConstantBackedSlugLiteral,
            });
        }
    }
    findings
}

fn collect_slug_constants(files: &[&FileFingerprint]) -> Vec<SlugConstant> {
    let mut constants = Vec::new();
    for fp in files {
        let class_name = CLASS_RE
            .captures(&fp.content)
            .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()));
        for cap in CONST_RE.captures_iter(&fp.content) {
            let const_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let value = cap.get(2).map(|m| m.as_str()).unwrap_or("");
            if !SLUG_VALUE_RE.is_match(value) {
                continue;
            }
            constants.push(SlugConstant {
                class_name: class_name.clone(),
                const_name: const_name.to_string(),
                value: value.to_string(),
                file: fp.relative_path.clone(),
            });
        }
    }
    constants
}

fn constant_label(constant: &SlugConstant) -> String {
    match &constant.class_name {
        Some(class_name) => format!("{}::{}", class_name, constant.const_name),
        None => constant.const_name.clone(),
    }
}

// ============================================================================
// #1561 — Option scope drift between docs and implementation
// ============================================================================

static SINGLE_SITE_OPTION_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b(get_option|update_option|delete_option)\s*\("#)
        .expect("single-site option regex compiles")
});

fn detect_option_scope_drift(files: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for fp in files {
        if !comments_promise_network_option_storage(fp) {
            continue;
        }
        for cap in SINGLE_SITE_OPTION_CALL_RE.captures_iter(&fp.content) {
            let full = cap.get(0).unwrap();
            let call = cap.get(1).map(|m| m.as_str()).unwrap_or("get_option");
            let line = line_of_offset(&fp.content, full.start());
            findings.push(Finding {
                convention: "requested_detectors".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Option scope drift at line {}: docs mention network/site-option storage but implementation calls `{}`",
                    line, call
                ),
                suggestion: format!(
                    "Use the matching `{}`site_option call or update the storage contract so multisite behaviour is explicit.",
                    if call.starts_with("delete") { "delete_" } else if call.starts_with("update") { "update_" } else { "get_" }
                ),
                kind: AuditFinding::OptionScopeDrift,
            });
        }
    }
    findings
}

fn comments_promise_network_option_storage(fp: &FileFingerprint) -> bool {
    comment_blocks::extract(fp).into_iter().any(|block| {
        let text = block.text.to_ascii_lowercase();
        if text.contains("not a network option") || text.contains("single-site option") {
            return false;
        }
        text.contains("network option")
            || text.contains("site option")
            || text.contains("multisite")
            || text.contains("shared across subsites")
            || text.contains("shared across sites")
    })
}

fn line_of_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn php_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn flags_json_like_exact_match_against_metadata_key() {
        let fp = php_fp(
            "inc/Core/Database/Chat.php",
            r#"<?php
$wpdb->get_results( "SELECT * FROM table WHERE metadata LIKE '%\"status\":\"processing\"%'" );
"#,
        );

        let findings = detect_json_like_exact_matches(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::JsonLikeExactMatch);
        assert!(findings[0].description.contains("metadata"));
        assert!(findings[0].description.contains("status"));
    }

    #[test]
    fn ignores_broad_like_search_that_does_not_match_json_key_semantics() {
        let fp = php_fp(
            "inc/Search.php",
            r#"<?php
$wpdb->get_results( "SELECT * FROM table WHERE metadata LIKE '%processing%'" );
"#,
        );

        assert!(detect_json_like_exact_matches(&[&fp]).is_empty());
    }

    #[test]
    fn flags_slug_literal_when_matching_constant_exists_elsewhere() {
        let constants = php_fp(
            "inc/Abilities/AbilityCategories.php",
            r#"<?php
final class AbilityCategories {
    public const CONTENT = 'datamachine-content';
}
"#,
        );
        let caller = php_fp(
            "inc/Abilities/Post/EditPostAbility.php",
            r#"<?php
register_ability( array( 'category' => 'datamachine-content' ) );
"#,
        );

        let findings = detect_constant_backed_slug_literals(&[&constants, &caller]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::ConstantBackedSlugLiteral);
        assert!(findings[0]
            .description
            .contains("AbilityCategories::CONTENT"));
    }

    #[test]
    fn ignores_non_slug_constants_and_declaring_file_literals() {
        let constants = php_fp(
            "inc/Status.php",
            r#"<?php
class Status {
    public const PROCESSING = 'processing';
    public const API = 'datamachine-api';
    public function value() { return 'datamachine-api'; }
}
"#,
        );
        let caller = php_fp(
            "inc/Other.php",
            r#"<?php
$status = 'processing';
"#,
        );

        assert!(detect_constant_backed_slug_literals(&[&constants, &caller]).is_empty());
    }

    #[test]
    fn flags_single_site_option_call_when_doc_promises_network_storage() {
        let fp = php_fp(
            "inc/Core/Auth/Callback.php",
            r#"<?php
/**
 * External tokens are stored in a network option shared across subsites.
 */
class Callback {
    public function save() {
        update_option( 'datamachine_external_tokens', array() );
    }
}
"#,
        );

        let findings = detect_option_scope_drift(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::OptionScopeDrift);
        assert!(findings[0].description.contains("update_option"));
    }

    #[test]
    fn ignores_matching_site_option_calls_and_single_site_docs() {
        let matching = php_fp(
            "inc/Core/OAuth/BaseProvider.php",
            r#"<?php
/** Auth data is stored in a network option. */
get_site_option( 'datamachine_auth_data', array() );
"#,
        );
        let single_site = php_fp(
            "inc/Core/Settings.php",
            r#"<?php
/** This value is intentionally a single-site option. */
get_option( 'datamachine_local_setting' );
"#,
        );

        assert!(detect_option_scope_drift(&[&matching, &single_site]).is_empty());
    }

    #[test]
    fn run_skips_vendored_php_files() {
        let fp = php_fp(
            "vendor/package/File.php",
            r#"<?php
/** Tokens use a network option. */
get_option( 'external_tokens' );
"#,
        );

        assert!(run(&[&fp]).is_empty());
    }
}
