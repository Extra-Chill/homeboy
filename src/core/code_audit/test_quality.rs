//! Test-quality audit detectors.
//!
//! These checks intentionally stay structural and conservative. They catch the
//! obvious cases that game coverage mapping without proving product behavior,
//! plus process-global env mutation guards that are unsafe under parallel tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use crate::code_audit::conventions::AuditFinding;
use crate::code_audit::findings::{Finding, Severity};
use crate::code_audit::walker::is_test_path;

pub(super) fn run(root: &Path) -> Vec<Finding> {
    let config = ScanConfig {
        extensions: ExtensionFilter::Only(vec!["rs".to_string()]),
        ..Default::default()
    };
    let files = codebase_scan::walk_files(root, &config);

    let mut findings = Vec::new();
    let mut env_mutations: BTreeMap<String, Vec<EnvMutationSite>> = BTreeMap::new();

    for file_path in files {
        let relative = match file_path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        if !is_test_path(&relative) {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&file_path) else {
            continue;
        };

        findings.extend(detect_vacuous_tests(&relative, &content));
        for site in detect_env_mutations(&relative, &content) {
            env_mutations
                .entry(site.var.clone())
                .or_default()
                .push(site);
        }
    }

    findings.extend(detect_inconsistent_env_guards(env_mutations));
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

#[derive(Debug)]
struct TestFunction {
    name: String,
    body: String,
}

fn detect_vacuous_tests(file: &str, content: &str) -> Vec<Finding> {
    let product_symbols = collect_product_imports(content);
    extract_test_functions(content)
        .into_iter()
        .filter_map(|test| vacuous_reason(&test, &product_symbols).map(|reason| (test, reason)))
        .map(|(test, reason)| Finding {
            convention: "test_quality".to_string(),
            severity: Severity::Info,
            file: file.to_string(),
            description: format!("Vacuous test `{}`: {}", test.name, reason),
            suggestion:
                "Delete the placeholder or replace it with a behavior test that calls product code"
                    .to_string(),
            kind: AuditFinding::VacuousTest,
        })
        .collect()
}

fn vacuous_reason(test: &TestFunction, product_symbols: &BTreeSet<String>) -> Option<&'static str> {
    if test.body.contains("compile contract") {
        return None;
    }

    let body = strip_comments(&test.body);
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();

    if compact == "assert!(true);" || compact == "std::assert!(true);" {
        return Some("body only asserts true");
    }

    if compact.contains("assert!(true);") && count_statements(&body) <= 1 {
        return Some("body only asserts true");
    }

    if !contains_assertion(&body) && !contains_product_reference(&body, product_symbols) {
        return Some("body has no assertion and no product-code reference");
    }

    if contains_assertion(&body)
        && !contains_product_reference(&body, product_symbols)
        && only_std_fixture_behavior(&body)
    {
        return Some("assertions exercise only stdlib or fixture behavior");
    }

    None
}

fn contains_assertion(body: &str) -> bool {
    body.contains("assert!")
        || body.contains("assert_eq!")
        || body.contains("assert_ne!")
        || body.contains("matches!")
}

fn contains_product_reference(body: &str, product_symbols: &BTreeSet<String>) -> bool {
    body.contains("homeboy::")
        || body.contains("crate::")
        || body.contains("super::")
        || body.contains("Command::cargo_bin")
        || product_symbols
            .iter()
            .any(|symbol| contains_word_call(body, symbol))
}

fn collect_product_imports(content: &str) -> BTreeSet<String> {
    let mut symbols = BTreeSet::new();
    let simple = regex::Regex::new(
        r"(?m)^\s*use\s+(?:homeboy|crate|super)::[^;]*::([A-Za-z_][A-Za-z0-9_]*)\s*;",
    )
    .unwrap();
    for cap in simple.captures_iter(content) {
        symbols.insert(cap[1].to_string());
    }

    let grouped =
        regex::Regex::new(r"(?m)^\s*use\s+(?:homeboy|crate|super)::[^;]*\{([^}]+)\}\s*;").unwrap();
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

fn contains_word_call(haystack: &str, needle: &str) -> bool {
    let pattern = format!(r"\b{}\s*\(", regex::escape(needle));
    regex::Regex::new(&pattern)
        .ok()
        .is_some_and(|re| re.is_match(haystack))
}

fn only_std_fixture_behavior(body: &str) -> bool {
    let body = body.trim();
    if body.is_empty() {
        return false;
    }

    let lower = body.to_ascii_lowercase();
    let std_markers = [
        "hashset",
        "hashmap",
        "btreeset",
        "btreemap",
        "tempfile",
        "tempdir",
        "std::",
        ".difference(",
        ".join(",
        ".exists(",
    ];
    let has_std_marker = std_markers.iter().any(|marker| lower.contains(marker));
    let has_product_like_call = lower.contains("homeboy")
        || lower.contains("component::")
        || lower.contains("rig::")
        || lower.contains("stack::")
        || lower.contains("audit::")
        || lower.contains("run_")
        || lower.contains("parse_")
        || lower.contains("validate_");

    has_std_marker && !has_product_like_call
}

fn count_statements(body: &str) -> usize {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .count()
}

fn strip_comments(body: &str) -> String {
    body.lines()
        .map(|line| {
            line.split_once("//")
                .map(|(before, _)| before)
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_test_functions(content: &str) -> Vec<TestFunction> {
    let lines: Vec<&str> = content.lines().collect();
    let mut tests = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if !lines[i].trim().starts_with("#[test]") {
            i += 1;
            continue;
        }

        let mut fn_line = i + 1;
        while fn_line < lines.len() && !lines[fn_line].contains("fn ") {
            fn_line += 1;
        }
        if fn_line >= lines.len() {
            break;
        }

        let Some(name) = extract_fn_name(lines[fn_line]) else {
            i = fn_line + 1;
            continue;
        };

        let mut depth = 0i32;
        let mut started = false;
        let mut body_lines = Vec::new();
        let mut j = fn_line;

        while j < lines.len() {
            let line = lines[j];
            if started {
                body_lines.push(line);
            }
            for ch in line.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        started = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if started && depth == 0 {
                break;
            }
            j += 1;
        }

        if let Some(last) = body_lines.last_mut() {
            if let Some((before, _)) = last.rsplit_once('}') {
                *last = before;
            }
        }

        tests.push(TestFunction {
            name,
            body: body_lines.join("\n"),
        });
        i = j + 1;
    }

    tests
}

fn extract_fn_name(line: &str) -> Option<String> {
    let after = line.split_once("fn ")?.1;
    let name = after
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()?;
    (!name.is_empty()).then(|| name.to_string())
}

#[derive(Debug, Clone)]
struct EnvMutationSite {
    file: String,
    var: String,
    uses_local_guard: bool,
    uses_shared_guard: bool,
}

fn detect_env_mutations(file: &str, content: &str) -> Vec<EnvMutationSite> {
    let mut vars = BTreeSet::new();
    for op in ["set_var", "remove_var"] {
        let needle = format!("std::env::{}(\"", op);
        for segment in content.split(&needle).skip(1) {
            if let Some((var, _)) = segment.split_once('"') {
                vars.insert(var.to_string());
            }
        }
    }

    let uses_local_guard = content.contains("fn home_lock")
        || content.contains("static HOME_LOCK")
        || content.contains("OnceLock<Mutex")
        || content.contains("struct HomeGuard");
    let uses_shared_guard = content.contains("test_support::")
        || content.contains("test_helpers::")
        || content.contains("shared_env")
        || content.contains("global_env_guard");

    vars.into_iter()
        .map(|var| EnvMutationSite {
            file: file.to_string(),
            var,
            uses_local_guard,
            uses_shared_guard,
        })
        .collect()
}

fn detect_inconsistent_env_guards(
    env_mutations: BTreeMap<String, Vec<EnvMutationSite>>,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    for (var, sites) in env_mutations {
        let files: BTreeSet<&str> = sites.iter().map(|site| site.file.as_str()).collect();
        if files.len() < 2 {
            continue;
        }

        let local_guard_count = sites
            .iter()
            .filter(|site| site.uses_local_guard && !site.uses_shared_guard)
            .count();
        if local_guard_count < 2 {
            continue;
        }

        let file_list = files.iter().copied().collect::<Vec<_>>().join(", ");
        for site in sites
            .iter()
            .filter(|site| site.uses_local_guard && !site.uses_shared_guard)
        {
            findings.push(Finding {
                convention: "test_quality".to_string(),
                severity: Severity::Warning,
                file: site.file.clone(),
                description: format!(
                    "Process-global env var `{}` is mutated behind a local guard; other mutating test files: {}",
                    var, file_list
                ),
                suggestion: format!(
                    "Move `{}` mutation behind one shared test-support guard used by every test file",
                    var
                ),
                kind: AuditFinding::DuplicateFunction,
            });
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path()).is_empty());
    }

    #[test]
    fn flags_assert_true_placeholder() {
        let findings = detect_vacuous_tests(
            "tests/commands/refactor_test.rs",
            r#"
#[test]
fn test_run() {
    assert!(true);
}
"#,
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::VacuousTest);
        assert!(findings[0].description.contains("asserts true"));
    }

    #[test]
    fn flags_stdlib_only_assertions_without_product_reference() {
        let findings = detect_vacuous_tests(
            "tests/commands/lint_test.rs",
            r#"
#[test]
fn test_count_newly_changed() {
    let a = HashSet::from(["a"]);
    let b = HashSet::from(["a", "b"]);
    assert_eq!(b.difference(&a).count(), 1);
}
"#,
        );

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("stdlib"));
    }

    #[test]
    fn keeps_real_product_tests_and_compile_contracts() {
        let findings = detect_vacuous_tests(
            "tests/commands/deploy_test.rs",
            r##"
#[test]
fn parse_bulk_component_ids_accepts_json() {
    let ids = crate::commands::deploy::parse_bulk_component_ids(r#"["a"]"#).unwrap();
    assert_eq!(ids, vec!["a"]);
}

#[test]
fn public_api_compiles() {
    // compile contract
    assert!(true);
}
"##,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_tests_that_call_imported_product_symbols() {
        let findings = detect_vacuous_tests(
            "tests/core/rig/check_test.rs",
            r#"
use crate::rig::check::evaluate;
use crate::rig::spec::{CheckSpec, RigSpec};

fn minimal_rig() -> RigSpec { todo!() }

#[test]
fn test_evaluate_file_exists() {
    let rig = minimal_rig();
    let spec = CheckSpec::default();
    evaluate(&rig, &spec).expect("existing file passes");
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn flags_repeated_local_home_guards() {
        let mut sites = BTreeMap::new();
        sites.insert(
            "HOME".to_string(),
            vec![
                detect_env_mutations(
                    "tests/core/rig/runner_test.rs",
                    r#"
static HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn home_lock() -> &'static Mutex<()> { HOME_LOCK.get_or_init(|| Mutex::new(())) }
fn with_isolated_home() { std::env::set_var("HOME", "/tmp/a"); }
"#,
                )
                .pop()
                .unwrap(),
                detect_env_mutations(
                    "tests/core/rig/install_test.rs",
                    r#"
struct HomeGuard;
impl HomeGuard { fn new() -> Self { std::env::set_var("HOME", "/tmp/b"); Self } }
"#,
                )
                .pop()
                .unwrap(),
            ],
        );

        let findings = detect_inconsistent_env_guards(sites);

        assert_eq!(findings.len(), 2);
        assert!(findings
            .iter()
            .all(|f| f.kind == AuditFinding::DuplicateFunction));
    }

    #[test]
    fn does_not_flag_single_file_or_shared_guard_env_mutation() {
        let mut single_file = BTreeMap::new();
        single_file.insert(
            "HOME".to_string(),
            vec![detect_env_mutations(
                "tests/core/rig/runner_test.rs",
                r#"
fn home_lock() {}
fn with_isolated_home() { std::env::set_var("HOME", "/tmp/a"); }
"#,
            )
            .pop()
            .unwrap()],
        );
        assert!(detect_inconsistent_env_guards(single_file).is_empty());

        let mut shared = BTreeMap::new();
        shared.insert(
            "HOME".to_string(),
            vec![
                detect_env_mutations(
                    "tests/a.rs",
                    r#"fn t() { test_support::global_env_guard(); std::env::set_var("HOME", "/tmp/a"); }"#,
                )
                .pop()
                .unwrap(),
                detect_env_mutations(
                    "tests/b.rs",
                    r#"fn t() { test_support::global_env_guard(); std::env::set_var("HOME", "/tmp/b"); }"#,
                )
                .pop()
                .unwrap(),
            ],
        );
        assert!(detect_inconsistent_env_guards(shared).is_empty());
    }
}
