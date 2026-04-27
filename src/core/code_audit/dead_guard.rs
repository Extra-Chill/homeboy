//! Reachability-aware dead-guard detector.
//!
//! Scans PHP file content for `function_exists('name')`, `class_exists('Name')`,
//! and `defined('CONST')` guards (and their negations) and emits a finding
//! when the checked symbol is guaranteed to exist given:
//!
//! 1. The plugin's declared requirements (`Requires at least:`).
//! 2. Unconditional `require` calls from the plugin main file.
//! 3. Known vendor packages declared in `composer.json`.
//!
//! The symbol-availability table is built by [`super::requirements`].

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::requirements::{known_available_symbols, KnownSymbols};
use crate::component::AuditConfig;

/// Kinds of guards we detect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardKind {
    Function,
    Class,
    Constant,
}

impl GuardKind {
    fn label(self) -> &'static str {
        match self {
            GuardKind::Function => "function_exists",
            GuardKind::Class => "class_exists",
            GuardKind::Constant => "defined",
        }
    }
}

struct Guard {
    kind: GuardKind,
    symbol: String,
    line: usize,
}

pub(super) fn run_with_config(
    fingerprints: &[&FileFingerprint],
    root: &Path,
    audit_config: &AuditConfig,
) -> Vec<Finding> {
    let known = known_available_symbols(root);
    if known.functions.is_empty() && known.classes.is_empty() && known.constants.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if fp.language != Language::Php {
            continue;
        }
        for guard in extract_guards(&fp.content) {
            if guard_is_contextual(fp, &guard, audit_config) {
                continue;
            }
            if symbol_is_known(&known, &guard) {
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
                        "Remove the {}('{}') guard; the symbol is guaranteed by plugin requirements, composer.json, or the plugin bootstrap",
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

fn symbol_is_known(known: &KnownSymbols, guard: &Guard) -> bool {
    match guard.kind {
        GuardKind::Function => known.has_function(&guard.symbol),
        GuardKind::Class => known.has_class(&guard.symbol),
        GuardKind::Constant => known.has_constant(&guard.symbol),
    }
}

fn guard_is_contextual(fp: &FileFingerprint, guard: &Guard, audit_config: &AuditConfig) -> bool {
    is_lifecycle_or_test_path(&fp.relative_path, audit_config)
        || guard_defines_stub(&fp.content, guard)
        || guard_loads_symbol_provider(&fp.content, guard)
}

fn is_lifecycle_or_test_path(path: &str, audit_config: &AuditConfig) -> bool {
    let normalized = path.replace('\\', "/");
    audit_config
        .lifecycle_path_globs
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, &normalized))
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
/// - `! class_exists( "WP_Ability" )`
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
        let line = line_of_offset(content, cap.get(0).map(|m| m.start()).unwrap_or(0));
        out.push(Guard { kind, symbol, line });
    }
    out
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
    use crate::code_audit::conventions::Language;
    use crate::code_audit::fingerprint::FileFingerprint;
    use std::fs;

    fn make_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn write_plugin_main(root: &Path, requires_at_least: Option<&str>, body: &str) {
        let header = requires_at_least
            .map(|v| format!(" * Requires at least: {}\n", v))
            .unwrap_or_default();
        let content = format!(
            "<?php\n/**\n * Plugin Name: Demo\n{} */\n\n{}",
            header, body
        );
        fs::write(root.join("plugin.php"), content).unwrap();
    }

    fn run(fingerprints: &[&FileFingerprint], root: &Path) -> Vec<Finding> {
        run_with_config(fingerprints, root, &AuditConfig::default())
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
    fn flags_dead_class_exists_for_wp_ability() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.9"), "");

        let fp = make_fp(
            "inc/Abilities/Register.php",
            r#"<?php
if ( ! class_exists( 'WP_Ability' ) ) {
    return;
}

class Register {}
"#,
        );

        let findings = run(&[&fp], tmp.path());
        assert_eq!(findings.len(), 1, "expected one dead-guard finding");
        assert_eq!(findings[0].kind, AuditFinding::DeadGuard);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].description.contains("WP_Ability"));
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

        let findings = run(&[&fp], tmp.path());
        assert!(
            findings.is_empty(),
            "unknown symbol should not be flagged, got: {:?}",
            findings
        );
    }

    #[test]
    fn action_scheduler_guard_becomes_dead_when_bootstrap_requires_it() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("vendor/woocommerce/action-scheduler")).unwrap();
        fs::write(
            tmp.path()
                .join("vendor/woocommerce/action-scheduler/action-scheduler.php"),
            "<?php\n",
        )
        .unwrap();

        write_plugin_main(
            tmp.path(),
            Some("6.0"),
            "require_once __DIR__ . '/vendor/woocommerce/action-scheduler/action-scheduler.php';\n",
        );

        let fp = make_fp(
            "inc/Scheduler.php",
            r#"<?php
if ( function_exists('as_schedule_single_action') ) {
    as_schedule_single_action( time(), 'my_hook' );
}
"#,
        );

        let findings = run(&[&fp], tmp.path());
        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .description
            .contains("as_schedule_single_action"));
    }

    #[test]
    fn non_php_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin_main(tmp.path(), Some("6.9"), "");

        let fp = FileFingerprint {
            relative_path: "src/lib.rs".to_string(),
            language: Language::Rust,
            content: "function_exists('WP_Ability')".to_string(),
            ..Default::default()
        };

        let findings = run(&[&fp], tmp.path());
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
        let findings = run(&[&fp], tmp.path());
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
            ..Default::default()
        };
        let guard = extract_guards(&fp.content).remove(0);
        assert!(
            guard_is_contextual(&fp, &guard, &config),
            "configured lifecycle globs mark guards as contextual"
        );
        let findings = run_with_config(&[&fp], tmp.path(), &config);
        assert!(
            findings.is_empty(),
            "uninstall context is not normal runtime"
        );
    }

    #[test]
    fn empty_known_symbols_short_circuits() {
        let tmp = tempfile::tempdir().unwrap();
        // No plugin main, no composer.json → known is empty.
        let fp = make_fp(
            "inc/X.php",
            r#"<?php if ( class_exists('WP_Ability') ) {} "#,
        );

        let findings = run(&[&fp], tmp.path());
        assert!(findings.is_empty());
    }
}
