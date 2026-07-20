//! Constant-bypass literal detection.
//!
//! Finds raw string literals whose value is byte-identical to a named string
//! constant already defined elsewhere in the codebase. The constant is the
//! canonical spelling — a hand-typed copy silently rots when the constant
//! changes (a schema-version bump, a renamed metadata key, a moved path), so
//! the copy should reference the constant instead.
//!
//! This is a first-pass, always-on detector. It complements the config-driven
//! `ConstantBackedSlugLiteral` (which only fires for constant patterns a
//! component explicitly registers) by discovering bypasses automatically from
//! the constants the codebase already defines.
//!
//! Language scope: the constant-declaration and literal scans use conservative
//! patterns that match the C-family `const NAME: T = "value"` /
//! `const NAME = "value"` shapes (Rust, TS, Go-ish, PHP `const`). It reads only
//! from `FileFingerprint::content`, so no fingerprint-model change is required.

use std::collections::HashMap;

use regex::Regex;

use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use super::super::walker::{cfg_test_regions, is_test_path, offset_in_cfg_test_region};

/// Minimum constant-value length worth flagging. Short values (`"workspace"`,
/// `"snapshot"`) collide with serde tags, enum spellings, and unrelated
/// domain words, so flagging them is noise. Schema/path/key strings — the
/// valuable cases — are comfortably longer.
const MIN_VALUE_LEN: usize = 12;

pub(crate) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    detect_constant_bypass_literals(fingerprints)
}

/// `const NAME: &str = "value";` and `const NAME = "value";` (and `static`).
/// Captures the constant name (1) and its string value (2).
fn const_decl_regex() -> &'static Regex {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"(?m)\b(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+([A-Z][A-Z0-9_]+)\s*(?::\s*&(?:'static\s+)?str\s*)?=\s*"([^"\\]{4,})"\s*;"#,
        )
        .expect("valid const-decl regex")
    });
    &RE
}

/// A recorded string constant: where it is defined and under what name.
struct ConstDef {
    name: String,
    file: String,
    /// 1-indexed line of the definition, so we never flag the definition itself.
    line: usize,
}

fn detect_constant_bypass_literals(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    // value -> definition. First definition wins; a value defined under two
    // names is itself a smell, but we report against one canonical name.
    let mut consts: HashMap<String, ConstDef> = HashMap::new();
    for fp in fingerprints {
        if is_test_path(&fp.relative_path) {
            continue;
        }
        for caps in const_decl_regex().captures_iter(&fp.content) {
            let name = caps[1].to_string();
            let value = caps[2].to_string();
            if value.len() < MIN_VALUE_LEN {
                continue;
            }
            let line = line_of_match(&fp.content, caps.get(0).unwrap().start());
            consts.entry(value).or_insert(ConstDef {
                name,
                file: fp.relative_path.clone(),
                line,
            });
        }
    }

    if consts.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if is_test_path(&fp.relative_path) {
            continue;
        }
        // Literals inside inline `#[cfg(test)]` blocks of a production file are
        // test data/fixtures, not production constant drift. `is_test_path` only
        // excludes whole test files, so skip matches inside cfg(test) regions.
        let test_regions = cfg_test_regions(&fp.content);
        for (offset, literal) in string_literals(&fp.content) {
            if offset_in_cfg_test_region(offset, &test_regions) {
                continue;
            }
            let Some(def) = consts.get(&literal) else {
                continue;
            };
            let line = line_of_match(&fp.content, offset);
            // Never flag the constant's own definition line.
            if fp.relative_path == def.file && line == def.line {
                continue;
            }
            // Skip the const-declaration line itself in any file (a re-export
            // `const OTHER: &str = SAME_VALUE;` is legitimate, though rare).
            if is_const_decl_line(&fp.content, offset) {
                continue;
            }
            findings.push(Finding {
                convention: "constant_bypass_literal".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: format!(
                    "Literal \"{}\" duplicates constant `{}` (defined in {})",
                    truncate(&literal),
                    def.name,
                    def.file
                ),
                suggestion: format!(
                    "Reference `{}` instead of hand-typing its value; editing the \
                     constant will not otherwise update this copy.",
                    def.name
                ),
                kind: AuditFinding::ConstantBypassLiteral,
            });
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });
    findings
}

/// Extract `"..."` string-literal values with their byte offset. A tolerant
/// character scanner that tracks string state and skips escapes, so quoted
/// braces/quotes inside a literal do not confuse it. Line and block comments
/// are ignored.
fn string_literals(content: &str) -> Vec<(usize, String)> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // line comment
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                let start = i;
                i += 1;
                let val_start = i;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
                if i <= bytes.len() && i > val_start {
                    if let Ok(val) = std::str::from_utf8(&bytes[val_start..i.min(bytes.len())]) {
                        // Only record literals with no escapes; an escaped value
                        // will not byte-match a plain constant string anyway.
                        if !val.contains('\\') {
                            out.push((start, val.to_string()));
                        }
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// 1-indexed line number of a byte offset.
fn line_of_match(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
}

/// Whether the line containing `offset` is itself a `const`/`static` string
/// declaration (so we don't flag the canonical definition or a re-export).
fn is_const_decl_line(content: &str, offset: usize) -> bool {
    let line_start = content[..offset.min(content.len())]
        .rfind('\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let line_end = content[offset.min(content.len())..]
        .find('\n')
        .map(|p| offset + p)
        .unwrap_or(content.len());
    let line = &content[line_start..line_end.min(content.len())];
    let t = line.trim_start();
    t.starts_with("const ")
        || t.starts_with("static ")
        || t.starts_with("pub const ")
        || t.starts_with("pub static ")
        || t.starts_with("pub(crate) const ")
        || t.starts_with("pub(crate) static ")
}

fn truncate(s: &str) -> String {
    if s.len() <= 48 {
        s.to_string()
    } else {
        format!("{}…", &s[..48])
    }
}

#[cfg(test)]
mod tests;
