//! Reachability-aware dead-guard detector.
//!
//! Scans guarded symbol-existence checks (`function_exists('name')`,
//! `class_exists('Name')`, `defined('CONST')` and their negations) and emits a
//! finding when the checked symbol is guaranteed to exist given the
//! component's known-symbol table.
//!
//! The languages the detector applies to and the lifecycle path/basename
//! policy are declared via [`DetectorProfileConfig`]; the symbol-availability
//! table is built by [`super::requirements`]. Core keeps no hardcoded
//! ecosystem manifest, file-suffix, or language literals here.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::requirements::{known_available_symbols, KnownSymbols};
use super::source_locations::line_of_offset;
use crate::core::component::AuditConfig;

/// Kinds of guards we detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardKind {
    Function,
    Class,
    Constant,
}

/// Static metadata for a [`GuardKind`] variant.
///
/// Centralizing the guard label and the matching `KnownSymbols` lookup in
/// one descriptor keeps variant additions localized: a new guard kind only
/// needs one new arm in [`GuardKind::descriptor`] instead of parallel arms
/// scattered across each getter / policy method.
struct GuardKindDescriptor {
    label: &'static str,
    has_symbol: fn(&KnownSymbols, &str) -> bool,
}

impl GuardKind {
    fn descriptor(self) -> GuardKindDescriptor {
        match self {
            GuardKind::Function => GuardKindDescriptor {
                label: "function_exists",
                has_symbol: |known, symbol| known.has_function(symbol),
            },
            GuardKind::Class => GuardKindDescriptor {
                label: "class_exists",
                has_symbol: |known, symbol| known.has_class(symbol),
            },
            GuardKind::Constant => GuardKindDescriptor {
                label: "defined",
                has_symbol: |known, symbol| known.has_constant(symbol),
            },
        }
    }

    fn label(self) -> &'static str {
        self.descriptor().label
    }

    fn symbol_is_known(self, known: &KnownSymbols, symbol: &str) -> bool {
        (self.descriptor().has_symbol)(known, symbol)
    }
}

struct Guard {
    kind: GuardKind,
    symbol: String,
    line: usize,
    offset: usize,
}

fn detect_with_config(
    fingerprints: &[&FileFingerprint],
    root: &Path,
    audit_config: &AuditConfig,
) -> Vec<Finding> {
    // Languages the guard syntax applies to are component-declared; core ships
    // no built-in ecosystem default here. Without a declared language set the
    // detector stays inert so its guard syntax never leaks an ecosystem
    // assumption into core.
    let guard_languages = &audit_config.detector_profile.dead_guard_languages;
    if guard_languages.is_empty() {
        return Vec::new();
    }

    let known = known_available_symbols(root, audit_config);
    if known.functions.is_empty() && known.classes.is_empty() && known.constants.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if !fp.language.matches_any_token(guard_languages) {
            continue;
        }
        for guard in extract_guards(&fp.content) {
            if guard_is_contextual(fp, &guard, audit_config) {
                continue;
            }
            if guard.kind.symbol_is_known(&known, &guard.symbol) {
                findings.push(Finding {
                    convention: "dead_guard".to_string(),
                    severity: Severity::Warning,
                    file: fp.relative_path.clone(),
                    description: format!(
                        "Dead guard on line {}: {}('{}') — symbol is guaranteed to exist at runtime",
                        guard.line,
                        guard.kind.label(),
                        guard.symbol
                    ),
                    suggestion: format!(
                        "Remove the {}('{}') guard; the symbol is guaranteed by the component's known-symbol table (runtime requirements, declared packages, or bootstrap)",
                        guard.kind.label(),
                        guard.symbol
                    ),
                    kind: AuditFinding::DeadGuard,
                });
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    root: &Path,
    audit_config: &AuditConfig,
) -> Vec<Finding> {
    detect_with_config(fingerprints, root, audit_config)
}

fn guard_is_contextual(fp: &FileFingerprint, guard: &Guard, audit_config: &AuditConfig) -> bool {
    is_lifecycle_or_test_path(&fp.relative_path, audit_config)
        || guard_has_context_comment(&fp.content, guard, audit_config)
        || guard_is_inside_registered_lifecycle_callback(&fp.content, guard)
        || guard_defines_stub(&fp.content, guard)
        || guard_loads_symbol_provider(&fp.content, guard)
}

fn guard_has_context_comment(content: &str, guard: &Guard, audit_config: &AuditConfig) -> bool {
    if audit_config.dead_guard_context_comment_patterns.is_empty() {
        return false;
    }
    let Some(comment_context) = guard_comment_context(content, guard) else {
        return false;
    };
    audit_config
        .dead_guard_context_comment_patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .any(|pattern| pattern.is_match(&comment_context))
}

fn guard_comment_context(content: &str, guard: &Guard) -> Option<String> {
    let start = preceding_comment_block_start_offset(content, guard.line)
        .unwrap_or_else(|| line_start_offset(content, guard.line));
    let end = guard_if_arm_end_offset(content, guard)
        .unwrap_or_else(|| line_end_offset(content, guard.line));
    let comments = extract_comments(&content[start..end.min(content.len())]);
    if comments.trim().is_empty() {
        None
    } else {
        Some(comments)
    }
}

fn preceding_comment_block_start_offset(content: &str, guard_line: usize) -> Option<usize> {
    let mut line = guard_line.checked_sub(1)?;
    let trimmed = line_text(content, line)?.trim();
    if trimmed.starts_with("*/") {
        loop {
            let current = line_text(content, line)?.trim();
            if current.contains("/*") || line == 1 {
                return Some(line_start_offset(content, line));
            }
            line -= 1;
        }
    }
    if !is_line_comment_like(trimmed) {
        return None;
    }
    while line > 1 {
        let Some(previous) = line_text(content, line - 1) else {
            break;
        };
        if !is_line_comment_like(previous.trim()) {
            break;
        }
        line -= 1;
    }
    Some(line_start_offset(content, line))
}

fn is_line_comment_like(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("*/")
}

fn guard_if_arm_end_offset(content: &str, guard: &Guard) -> Option<usize> {
    let remainder = content.get(guard.offset..)?;
    let brace_offset = remainder.find('{')? + guard.offset;
    let semicolon_offset = remainder.find(';').map(|offset| offset + guard.offset);
    if semicolon_offset.is_some_and(|semicolon| semicolon < brace_offset) {
        return None;
    }
    matching_brace_offset(content, brace_offset).map(|offset| offset + 1)
}

fn extract_comments(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut comments = String::new();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b'/' && bytes.get(idx + 1) == Some(&b'/') {
            let start = idx + 2;
            let end = content[start..]
                .find('\n')
                .map(|offset| start + offset)
                .unwrap_or(content.len());
            comments.push_str(&content[start..end]);
            comments.push('\n');
            idx = end;
        } else if bytes[idx] == b'#' {
            let start = idx + 1;
            let end = content[start..]
                .find('\n')
                .map(|offset| start + offset)
                .unwrap_or(content.len());
            comments.push_str(&content[start..end]);
            comments.push('\n');
            idx = end;
        } else if bytes[idx] == b'/' && bytes.get(idx + 1) == Some(&b'*') {
            let start = idx + 2;
            let end = content[start..]
                .find("*/")
                .map(|offset| start + offset)
                .unwrap_or(content.len());
            comments.push_str(&content[start..end]);
            comments.push('\n');
            idx = end.saturating_add(2);
        } else {
            idx += 1;
        }
    }
    comments
}

fn is_lifecycle_or_test_path(path: &str, audit_config: &AuditConfig) -> bool {
    let normalized = path.replace('\\', "/");
    let profile = &audit_config.detector_profile;
    if is_declared_contextual_path(
        &normalized,
        &profile.lifecycle_path_segments,
        &profile.lifecycle_basenames,
        &profile.lifecycle_basename_suffixes,
    ) {
        return true;
    }
    audit_config
        .lifecycle_path_globs
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, &normalized))
}

/// Whether the path is contextual (lifecycle/migration/test) per the
/// component-declared catalogues. Core owns only the generic matching: a path
/// segment match, an exact basename match, or a basename-suffix match. The
/// concrete directory names, filenames, and suffixes (which carry ecosystem
/// file-extension assumptions) are declared via `DetectorProfileConfig`.
fn is_declared_contextual_path(
    path: &str,
    segments: &[String],
    basenames: &[String],
    basename_suffixes: &[String],
) -> bool {
    let lower = path.to_ascii_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or(lower.as_str());

    let segment_match = segments.iter().any(|segment| {
        let seg = segment.to_ascii_lowercase();
        lower.starts_with(&format!("{seg}/")) || lower.contains(&format!("/{seg}/"))
    });
    if segment_match {
        return true;
    }
    if basenames
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(basename))
    {
        return true;
    }
    basename_suffixes
        .iter()
        .any(|suffix| basename.ends_with(&suffix.to_ascii_lowercase()))
}

fn guard_is_inside_registered_lifecycle_callback(content: &str, guard: &Guard) -> bool {
    let callbacks = lifecycle_callbacks(content);
    if callbacks.is_empty() {
        return false;
    }
    let guard_offset = line_start_offset(content, guard.line);
    function_name_at_offset(content, guard_offset)
        .as_deref()
        .is_some_and(|name| callbacks.contains(name))
}

fn lifecycle_callbacks(content: &str) -> HashSet<String> {
    lifecycle_hook_regex()
        .captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn function_name_at_offset(content: &str, offset: usize) -> Option<String> {
    for cap in function_declaration_regex().captures_iter(content) {
        let name = cap.get(1)?.as_str();
        let body_start = cap.get(0)?.end().saturating_sub(1);
        let body_end = matching_brace_offset(content, body_start)?;
        if body_start <= offset && offset <= body_end {
            return Some(name.to_string());
        }
    }
    None
}

fn matching_brace_offset(content: &str, open_offset: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes.get(open_offset) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    for (idx, byte) in bytes.iter().enumerate().skip(open_offset) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn guard_defines_stub(content: &str, guard: &Guard) -> bool {
    if guard.kind != GuardKind::Function {
        return false;
    }
    let pattern = format!(r"(?m)\bfunction\s+{}\s*\(", regex::escape(&guard.symbol));
    Regex::new(&pattern)
        .map(|re| re.is_match(content))
        .unwrap_or(false)
}

fn guard_loads_symbol_provider(content: &str, guard: &Guard) -> bool {
    if guard.kind != GuardKind::Class {
        return false;
    }
    let symbol = guard.symbol.to_ascii_lowercase();
    let content = content.to_ascii_lowercase();
    content.contains("require")
        && (content.contains(&symbol)
            || content.contains(&symbol.replace('_', "-"))
            || content.contains(&camel_to_kebab(&guard.symbol)))
}

fn camel_to_kebab(symbol: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in symbol.chars().enumerate() {
        if ch.is_ascii_uppercase() && idx > 0 {
            out.push('-');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Regex matching any of the three guard calls plus a quoted symbol argument.
///
/// Examples matched:
/// - `function_exists('foo_bar')`
/// - `! class_exists( "RuntimeCapability" )`
/// - `defined('RUNTIME_REQUEST')`
///
/// Group 1: guard name. Group 2 or 3: quoted symbol (without quotes).
fn guards_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?x)
            \b(function_exists|class_exists|defined)\s*
            \(\s*
            (?:'([^'\\]+)'|"([^"\\]+)")
            \s*\)
            "#,
        )
        .expect("dead_guard regex compiles")
    })
}

fn lifecycle_hook_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?is)\bregister_(?:activation|deactivation|uninstall)_hook\s*\([^;]*?["']([A-Za-z_][A-Za-z0-9_]*)["']"#,
        )
        .expect("lifecycle callback regex compiles")
    })
}

fn function_declaration_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)\bfunction\s+([A-Za-z_][A-Za-z0-9_]*)\s*\([^)]*\)\s*\{"#)
            .expect("function declaration regex compiles")
    })
}

fn extract_guards(content: &str) -> Vec<Guard> {
    let re = guards_regex();
    let mut out = Vec::new();
    for cap in re.captures_iter(content) {
        let guard_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let symbol = cap
            .get(2)
            .or_else(|| cap.get(3))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if symbol.is_empty() {
            continue;
        }
        let kind = match guard_name {
            "function_exists" => GuardKind::Function,
            "class_exists" => GuardKind::Class,
            "defined" => GuardKind::Constant,
            _ => continue,
        };
        let offset = cap.get(0).map(|m| m.start()).unwrap_or(0);
        let line = line_of_offset(content, offset);
        out.push(Guard {
            kind,
            symbol,
            line,
            offset,
        });
    }
    out
}

fn line_start_offset(content: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut current_line = 1usize;
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            current_line += 1;
            if current_line == line {
                return idx + 1;
            }
        }
    }
    content.len()
}

fn line_end_offset(content: &str, line: usize) -> usize {
    let start = line_start_offset(content, line);
    content[start..]
        .find('\n')
        .map(|offset| start + offset)
        .unwrap_or(content.len())
}

fn line_text(content: &str, line: usize) -> Option<&str> {
    if line == 0 {
        return None;
    }
    let start = line_start_offset(content, line);
    if start >= content.len() {
        return None;
    }
    let end = line_end_offset(content, line);
    Some(&content[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::Language;
    use crate::core::code_audit::fingerprint::FileFingerprint;
    use std::fs;

    fn make_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    /// Representative dead-guard profile mirroring what a PHP/WordPress
    /// extension would declare: the guard syntax applies to PHP, and the
    /// lifecycle catalogue covers the usual non-runtime entrypoints.
    fn test_profile_json() -> serde_json::Value {
        serde_json::json!({
            "dead_guard_languages": ["php"],
            "lifecycle_path_segments": [
                "migrations", "migration", "tests", "test", "fixtures", "smoke"
            ],
            "lifecycle_basenames": [
                "uninstall.php", "activation.php", "deactivation.php", "smoke.php"
            ],
            "lifecycle_basename_suffixes": ["-smoke.php", "_smoke.php"]
        })
    }

    fn test_config() -> AuditConfig {
        serde_json::from_value(serde_json::json!({
            "detector_profile": test_profile_json(),
            "known_symbols": {
                "header_versions": [
                    {
                        "file_marker": "Runtime Plugin:",
                        "version_header": "Runtime Requires:",
                        "symbols": [
                            {"name": "RuntimeCapability", "kind": "class", "introduced": "2.4"},
                            {"name": "runtime_json_encode", "kind": "function", "introduced": "1.0"},
                            {"name": "runtime_unschedule_all", "kind": "function", "introduced": "1.0"}
                        ]
                    }
                ],
                "bootstrap_paths": [
                    {
                        "path_contains": "runtime-queue/runtime-queue.php",
                        "symbols": [
                            {"name": "runtime_schedule_once", "kind": "function"}
                        ]
                    }
                ],
                "source_scan": {
                    "entry_file_extensions": ["php"],
                    "require_keywords": ["require_once", "require", "include_once", "include"],
                    "guard_markers": ["class_exists", "function_exists", "defined"]
                }
            }
        }))
        .unwrap()
    }

    fn write_plugin_main(root: &Path, requires_at_least: Option<&str>, body: &str) {
        let header = requires_at_least
            .map(|v| format!(" * Runtime Requires: {}\n", v))
            .unwrap_or_default();
        let content = format!(
            "<?php\n/**\n * Runtime Plugin: Demo\n{} */\n\n{}",
            header, body
        );
        fs::write(root.join("plugin.php"), content).unwrap();
    }

    fn detect_for_test(fingerprints: &[&FileFingerprint], root: &Path) -> Vec<Finding> {
        detect_with_config(fingerprints, root, &test_config())
    }

    #[test]
    fn extract_guards_finds_all_three_kinds() {
        let content = r#"<?php
if ( function_exists('runtime_now') ) {}
if ( ! class_exists( 'RuntimeCapability' ) ) {}
if ( defined("RUNTIME_REQUEST") ) {}
"#;
        let guards = extract_guards(content);
        assert_eq!(guards.len(), 3);
        assert_eq!(guards[0].kind, GuardKind::Function);
        assert_eq!(guards[0].symbol, "runtime_now");
        assert_eq!(guards[1].kind, GuardKind::Class);
        assert_eq!(guards[1].symbol, "RuntimeCapability");
        assert_eq!(guards[2].kind, GuardKind::Constant);
        assert_eq!(guards[2].symbol, "RUNTIME_REQUEST");
    }

    #[test]
    fn flags_dead_class_exists_for_known_runtime_symbol() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("2.4"), "");

        let fp = make_fp(
            "inc/Abilities/Register.php",
            r#"<?php
if ( ! class_exists( 'RuntimeCapability' ) ) {
    return;
}

class Register {}
"#,
        );

        let findings = detect_for_test(&[&fp], tmp.path());
        assert_eq!(findings.len(), 1, "expected one dead-guard finding");
        assert_eq!(findings[0].kind, AuditFinding::DeadGuard);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].description.contains("RuntimeCapability"));
        assert!(findings[0].description.contains("class_exists"));
    }

    #[test]
    fn does_not_flag_unknown_function() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.9"), "");

        let fp = make_fp(
            "inc/Bootstrap.php",
            r#"<?php
if ( ! function_exists('my_plugin_helper') ) {
    function my_plugin_helper() {}
}
"#,
        );

        let findings = detect_for_test(&[&fp], tmp.path());
        assert!(
            findings.is_empty(),
            "unknown symbol should not be flagged, got: {:?}",
            findings
        );
    }

    #[test]
    fn configured_bootstrap_provider_guard_becomes_dead_when_required() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("vendor/runtime-queue")).unwrap();
        fs::write(
            tmp.path().join("vendor/runtime-queue/runtime-queue.php"),
            "<?php\n",
        )
        .unwrap();

        write_plugin_main(
            tmp.path(),
            Some("2.0"),
            "require_once __DIR__ . '/vendor/runtime-queue/runtime-queue.php';\n",
        );

        let fp = make_fp(
            "inc/Scheduler.php",
            r#"<?php
if ( function_exists('runtime_schedule_once') ) {
    runtime_schedule_once( time(), 'my_hook' );
}
"#,
        );

        let findings = detect_for_test(&[&fp], tmp.path());
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("runtime_schedule_once"));
    }

    #[test]
    fn non_php_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("2.4"), "");

        let fp = FileFingerprint {
            relative_path: "src/lib.rs".to_string(),
            language: Language::Rust,
            content: "function_exists('RuntimeCapability')".to_string(),
            ..Default::default()
        };

        let findings = detect_for_test(&[&fp], tmp.path());
        assert!(findings.is_empty());
    }

    #[test]
    fn test_stub_definition_guard_is_not_dead() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "tests/compat-smoke.php",
            r#"<?php
if ( ! function_exists('runtime_json_encode') ) {
    function runtime_json_encode( $value ) { return json_encode( $value ); }
}
"#,
        );

        let guard = extract_guards(&fp.content).remove(0);
        assert!(
            guard_is_contextual(&fp, &guard, &AuditConfig::default()),
            "stub definition guards are contextual even when the symbol is otherwise available"
        );
        let findings = detect_for_test(&[&fp], tmp.path());
        assert!(findings.is_empty(), "stub guards are test scaffolding");
    }

    #[test]
    fn configured_lifecycle_paths_are_not_production_dead_guards() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "lifecycle/teardown.php",
            r#"<?php
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );

        let config = AuditConfig {
            lifecycle_path_globs: vec!["lifecycle/*.php".to_string()],
            known_symbols: test_config().known_symbols,
            detector_profile: test_config().detector_profile,
            ..Default::default()
        };
        let guard = extract_guards(&fp.content).remove(0);
        assert!(
            guard_is_contextual(&fp, &guard, &config),
            "configured lifecycle globs mark guards as contextual"
        );
        let findings = detect_with_config(&[&fp], tmp.path(), &config);
        assert!(
            findings.is_empty(),
            "uninstall context is not normal runtime"
        );
    }

    #[test]
    fn default_lifecycle_paths_are_not_production_dead_guards() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");

        let migration = make_fp(
            "inc/migrations/flows.php",
            r#"<?php
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );
        let uninstall = make_fp(
            "uninstall.php",
            r#"<?php
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );
        let smoke = make_fp(
            "tests/runtime-smoke.php",
            r#"<?php
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );

        let findings = detect_for_test(&[&migration, &uninstall, &smoke], tmp.path());
        assert!(
            findings.is_empty(),
            "migration, uninstall, and smoke contexts are not normal runtime: {:?}",
            findings
        );
    }

    #[test]
    fn registered_deactivation_callback_is_not_a_production_dead_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let body = r#"
function runtime_deactivate_plugin() {
    if ( function_exists('runtime_unschedule_all') ) {
        runtime_unschedule_all('demo');
    }
}

function runtime_normal_request() {
    if ( function_exists('runtime_unschedule_all') ) {
        runtime_unschedule_all('demo');
    }
}

register_deactivation_hook( __FILE__, 'runtime_deactivate_plugin' );
"#;
        write_plugin_main(tmp.path(), Some("6.0"), body);
        let content = fs::read_to_string(tmp.path().join("plugin.php")).unwrap();
        let fp = make_fp("plugin.php", &content);

        let findings = detect_for_test(&[&fp], tmp.path());
        assert_eq!(
            findings.len(),
            1,
            "only the normal request guard should remain reportable: {:?}",
            findings
        );
        assert!(findings[0].description.contains("runtime_unschedule_all"));
    }

    #[test]
    fn production_guard_on_known_symbol_still_reports() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "inc/RuntimeScheduler.php",
            r#"<?php
function runtime_normal_request() {
    if ( function_exists('runtime_unschedule_all') ) {
        runtime_unschedule_all('demo');
    }
}
"#,
        );

        let findings = detect_for_test(&[&fp], tmp.path());
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("runtime_unschedule_all"));
    }

    #[test]
    fn configured_comment_context_suppresses_dead_guard() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "inc/DualContext.php",
            r#"<?php
// Fallback when runtime is not loaded by the smoke harness.
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );
        let config = AuditConfig {
            dead_guard_context_comment_patterns: vec![
                "(?i)runtime is not loaded by the smoke harness".to_string(),
            ],
            known_symbols: test_config().known_symbols,
            detector_profile: test_config().detector_profile,
            ..Default::default()
        };

        let findings = detect_with_config(&[&fp], tmp.path(), &config);
        assert!(findings.is_empty(), "matched comment context is exempt");
    }

    #[test]
    fn configured_comment_context_can_match_guard_arm_comments() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "inc/DualContext.php",
            r#"<?php
if ( function_exists('runtime_unschedule_all') ) {
    // Dual-context fallback path for the standalone runner.
    runtime_unschedule_all('demo');
}
"#,
        );
        let config = AuditConfig {
            dead_guard_context_comment_patterns: vec!["standalone runner".to_string()],
            known_symbols: test_config().known_symbols,
            detector_profile: test_config().detector_profile,
            ..Default::default()
        };

        let findings = detect_with_config(&[&fp], tmp.path(), &config);
        assert!(findings.is_empty(), "guard-arm comment context is exempt");
    }

    #[test]
    fn unconfigured_comment_context_still_reports_dead_guard() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "inc/DualContext.php",
            r#"<?php
// Fallback when runtime is not loaded by the smoke harness.
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );

        let findings = detect_for_test(&[&fp], tmp.path());
        assert_eq!(findings.len(), 1, "comments are not magic without config");
    }

    #[test]
    fn configured_comment_context_ignores_non_comment_text() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.0"), "");
        let fp = make_fp(
            "inc/DualContext.php",
            r#"<?php
$message = 'standalone runner';
if ( function_exists('runtime_unschedule_all') ) {
    runtime_unschedule_all('demo');
}
"#,
        );
        let config = AuditConfig {
            dead_guard_context_comment_patterns: vec!["standalone runner".to_string()],
            known_symbols: test_config().known_symbols,
            detector_profile: test_config().detector_profile,
            ..Default::default()
        };

        let findings = detect_with_config(&[&fp], tmp.path(), &config);
        assert_eq!(findings.len(), 1, "only comment context is matched");
    }

    #[test]
    fn empty_known_symbols_short_circuits() {
        let tmp = tempfile::tempdir().unwrap();
        // No plugin main, no composer.json → known is empty.
        let fp = make_fp(
            "inc/X.php",
            r#"<?php if ( class_exists('RuntimeCapability') ) {} "#,
        );

        let findings = detect_for_test(&[&fp], tmp.path());
        assert!(findings.is_empty());
    }
}
