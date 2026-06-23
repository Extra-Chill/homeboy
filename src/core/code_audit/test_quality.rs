//! Test-quality audit detectors.
//!
//! These checks intentionally stay structural and conservative. They catch the
//! obvious cases that game coverage mapping without proving product behavior,
//! plus process-global env mutation guards that are unsafe under parallel tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::core::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};

use crate::core::code_audit::conventions::AuditFinding;
use crate::core::code_audit::findings::{Finding, Severity};
use crate::core::code_audit::walker::is_test_path;

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
    line: usize,
    nested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProductImport {
    symbol: String,
}

fn detect_vacuous_tests(file: &str, content: &str) -> Vec<Finding> {
    let file_path = file.to_string();
    let inline_source_file = !file.starts_with("tests/") && !file.ends_with("_test.rs");
    let product_imports = collect_product_imports(content);
    let mut product_symbols = product_imports
        .iter()
        .map(|import| import.symbol.clone())
        .collect::<BTreeSet<_>>();
    if inline_source_file {
        product_symbols.extend(collect_local_product_symbols(content));
    }
    let tests = extract_test_functions(content);

    let mut findings = tests
        .iter()
        .filter(|test| test.nested)
        .map(|test| Finding {
            convention: "test_quality".to_string(),
            severity: Severity::Info,
            file: file_path.clone(),
            description: format!(
                "Unreachable test `{}` is nested inside another item near line {}; the test harness will not discover it",
                test.name, test.line
            ),
            suggestion: format!(
                "Move `{}` to module scope so the test harness can execute it",
                test.name
            ),
            kind: AuditFinding::VacuousTest,
        })
        .collect::<Vec<_>>();

    findings.extend(detect_duplicate_test_names(&file_path, &tests));
    if !inline_source_file {
        findings.extend(detect_unused_product_imports(
            &file_path,
            content,
            &tests,
            &product_imports,
        ));
    }

    findings.extend(tests.into_iter().filter_map(|test| {
        vacuous_reason(&test, &product_symbols).map(|reason| Finding {
            convention: "test_quality".to_string(),
            severity: Severity::Info,
            file: file_path.clone(),
            description: format!("Vacuous test `{}`: {}", test.name, reason),
            suggestion:
                "Delete the placeholder or replace it with a behavior test that calls product code"
                    .to_string(),
            kind: AuditFinding::VacuousTest,
        })
    }));

    findings
}

fn collect_product_imports(content: &str) -> Vec<ProductImport> {
    let mut imports = BTreeSet::new();
    let module =
        regex::Regex::new(r"(?m)^\s*use\s+(?:homeboy|crate|super)::([A-Za-z_][A-Za-z0-9_]*)\s*;")
            .unwrap();
    for cap in module.captures_iter(content) {
        imports.insert(ProductImport {
            symbol: cap[1].to_string(),
        });
    }

    let simple = regex::Regex::new(
        r"(?m)^\s*use\s+(?:homeboy|crate|super)::[^;]*::([A-Za-z_][A-Za-z0-9_]*)\s*;",
    )
    .unwrap();
    for cap in simple.captures_iter(content) {
        imports.insert(ProductImport {
            symbol: cap[1].to_string(),
        });
    }

    let grouped =
        regex::Regex::new(r"(?m)^\s*use\s+(homeboy|crate|super)::([^;{]+)::\{([^}]+)\}\s*;")
            .unwrap();
    for cap in grouped.captures_iter(content) {
        let root = &cap[1];
        let prefix = cap[2].rsplit("::").next().map(str::trim).unwrap_or("");
        for raw in cap[3].split(',') {
            let symbol = imported_symbol(raw);
            let symbol = if symbol == "self" && root == "homeboy" {
                prefix
            } else {
                symbol.as_str()
            };
            if !symbol.is_empty()
                && symbol != "self"
                && symbol
                    .chars()
                    .all(|c| c == '_' || c.is_ascii_alphanumeric())
            {
                imports.insert(ProductImport {
                    symbol: symbol.to_string(),
                });
            }
        }
    }

    let root_grouped =
        regex::Regex::new(r"(?m)^\s*use\s+(homeboy|crate|super)::\{([^}]+)\}\s*;").unwrap();
    for cap in root_grouped.captures_iter(content) {
        for raw in cap[2].split(',') {
            let symbol = imported_symbol(raw);
            if !symbol.is_empty()
                && symbol != "self"
                && symbol
                    .chars()
                    .all(|c| c == '_' || c.is_ascii_alphanumeric())
            {
                imports.insert(ProductImport {
                    symbol: symbol.to_string(),
                });
            }
        }
    }

    imports.into_iter().collect()
}

fn imported_symbol(raw: &str) -> String {
    let symbol = raw.trim().trim_start_matches("self::");
    if let Some((_, alias)) = symbol.split_once(" as ") {
        alias.trim().to_string()
    } else {
        symbol.split_whitespace().next().unwrap_or("").to_string()
    }
}

fn collect_local_product_symbols(content: &str) -> BTreeSet<String> {
    let mut symbols = BTreeSet::new();
    let product_content = content.split("#[cfg(test)]").next().unwrap_or(content);
    let item = regex::Regex::new(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|type|trait)\s+([A-Za-z_][A-Za-z0-9_]*)\b",
    )
    .unwrap();
    for cap in item.captures_iter(product_content) {
        symbols.insert(cap[1].to_string());
    }
    symbols
}

fn detect_duplicate_test_names(file: &str, tests: &[TestFunction]) -> Vec<Finding> {
    let mut seen = BTreeMap::<&str, usize>::new();
    let mut findings = Vec::new();

    for test in tests {
        if let Some(first_line) = seen.insert(&test.name, test.line) {
            findings.push(Finding {
                convention: "test_quality".to_string(),
                severity: Severity::Info,
                file: file.to_string(),
                description: format!(
                    "Duplicate test name `{}` at line {} shadows earlier coverage from line {}",
                    test.name, test.line, first_line
                ),
                suggestion: format!(
                    "Rename one `{}` test so each behavior has distinct coverage",
                    test.name
                ),
                kind: AuditFinding::VacuousTest,
            });
        }
    }

    findings
}

fn detect_unused_product_imports(
    file: &str,
    content: &str,
    tests: &[TestFunction],
    imports: &[ProductImport],
) -> Vec<Finding> {
    if imports.is_empty() || tests.is_empty() {
        return Vec::new();
    }

    let stripped_body = strip_use_lines(&strip_comments(content));

    imports
        .iter()
        .filter(|import| {
            import
                .symbol
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_lowercase())
        })
        .filter(|import| !contains_symbol_reference(&stripped_body, &import.symbol))
        .map(|import| Finding {
            convention: "test_quality".to_string(),
            severity: Severity::Info,
            file: file.to_string(),
            description: format!(
                "Imported product symbol `{}` is never exercised by any test body",
                import.symbol
            ),
            suggestion: format!(
                "Call `{}` in a behavior assertion, or remove the misleading import",
                import.symbol
            ),
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
            .any(|symbol| contains_symbol_reference(body, symbol))
}

fn contains_symbol_reference(body: &str, symbol: &str) -> bool {
    let call_pattern = format!(r"\b{}\s*\(", regex::escape(symbol));
    let path_pattern = format!(r"\b{}::", regex::escape(symbol));
    regex::Regex::new(&call_pattern)
        .ok()
        .is_some_and(|re| re.is_match(body))
        || regex::Regex::new(&path_pattern)
            .ok()
            .is_some_and(|re| re.is_match(body))
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

fn strip_use_lines(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim_start().starts_with("use "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_test_functions(content: &str) -> Vec<TestFunction> {
    let lines: Vec<&str> = content.lines().collect();
    let mut tests = Vec::new();
    let mut i = 0;
    let mut enclosing_depth = 0i32;
    let mut function_end_depths = Vec::new();

    while i < lines.len() {
        if !lines[i].trim().starts_with("#[test]") {
            update_enclosing_context(lines[i], &mut enclosing_depth, &mut function_end_depths);
            i += 1;
            continue;
        }

        let nested = !function_end_depths.is_empty();
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
            line: fn_line + 1,
            nested,
        });
        for line in &lines[i..=j.min(lines.len().saturating_sub(1))] {
            enclosing_depth += brace_delta(line);
        }
        pop_closed_functions(enclosing_depth, &mut function_end_depths);
        i = j + 1;
    }

    tests
}

fn update_enclosing_context(line: &str, depth: &mut i32, function_end_depths: &mut Vec<i32>) {
    let code = line.split_once("//").map(|(code, _)| code).unwrap_or(line);
    let opens = code.chars().filter(|ch| *ch == '{').count() as i32;
    let closes = code.chars().filter(|ch| *ch == '}').count() as i32;
    if code.contains("fn ") && opens > closes {
        function_end_depths.push(*depth + 1);
    }
    *depth += opens - closes;
    pop_closed_functions(*depth, function_end_depths);
}

fn pop_closed_functions(depth: i32, function_end_depths: &mut Vec<i32>) {
    while function_end_depths
        .last()
        .is_some_and(|end_depth| depth < *end_depth)
    {
        function_end_depths.pop();
    }
}

fn brace_delta(line: &str) -> i32 {
    let code = line.split_once("//").map(|(code, _)| code).unwrap_or(line);
    code.chars().fold(0, |delta, ch| match ch {
        '{' => delta + 1,
        '}' => delta - 1,
        _ => delta,
    })
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
use crate::core::rig::check::evaluate;
use crate::core::rig::spec::{CheckSpec, RigSpec};

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
    fn keeps_module_import_product_calls() {
        let findings = detect_vacuous_tests(
            "tests/deps_test.rs",
            r#"
use homeboy::core::deps::{self, ComposerAction};

#[test]
fn status_filters_to_one_package() {
    let status = deps::status(Some("fixture"), Some("/tmp/fixture"), Some("fixture/two")).unwrap();
    assert_eq!(status.packages.len(), 1);
}

#[test]
fn update_command_args_are_composer_first_and_package_scoped() {
    assert_eq!(
        deps::composer_command_args("fixture/package", &ComposerAction::Update),
        vec!["update", "fixture/package"]
    );
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_inline_super_import_tests_that_call_local_product_code() {
        let findings = detect_vacuous_tests(
            "src/core/extension/test/baseline.rs",
            r#"
pub fn load_baseline(path: &Path) -> Option<TestBaseline> { todo!() }
pub fn compare(current: &TestCounts, baseline: &TestBaseline) -> TestBaselineComparison { todo!() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_baseline(dir.path()).is_none());
    }

    #[test]
    fn compare_no_regression() {
        let result = compare(&current, &baseline);
        assert!(!result.regression);
    }
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn flags_nested_tests_that_runner_will_not_execute() {
        let findings = detect_vacuous_tests(
            "tests/core/wiring_test.rs",
            r#"
fn helper() {
    #[test]
    fn nested_test() {
        assert!(true);
    }
}
"#,
        );

        assert!(findings.iter().any(|finding| finding
            .description
            .contains("Unreachable test `nested_test`")));
    }

    #[test]
    fn keeps_module_scoped_tests_as_reachable() {
        let findings = detect_vacuous_tests(
            "tests/core/wiring_test.rs",
            r#"
mod behavior {
    #[test]
    fn module_test() {
        crate::thing::run();
    }
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn flags_duplicate_test_names_as_shadowed_coverage() {
        let findings = detect_vacuous_tests(
            "tests/core/wiring_test.rs",
            r#"
#[test]
fn duplicate_behavior() {
    crate::thing::run();
}

#[test]
fn duplicate_behavior() {
    crate::thing::run_again();
}
"#,
        );

        assert!(findings.iter().any(|finding| finding
            .description
            .contains("Duplicate test name `duplicate_behavior`")));
    }

    #[test]
    fn flags_product_imports_never_exercised_by_test_body() {
        let content = r#"
use crate::core::target::run_target;

#[test]
fn fixture_only() {
    let values = std::collections::HashSet::from(["a", "b"]);
    assert_eq!(values.len(), 2);
}
"#;
        let imports = collect_product_imports(content);
        assert_eq!(imports[0].symbol, "run_target");
        let tests = extract_test_functions(content);
        assert_eq!(tests.len(), 1);
        let import_findings =
            detect_unused_product_imports("tests/core/wiring_test.rs", content, &tests, &imports);
        assert_eq!(import_findings.len(), 1);

        let findings = detect_vacuous_tests("tests/core/wiring_test.rs", content);

        assert!(findings.iter().any(|finding| finding
            .description
            .contains("Imported product symbol `run_target`")));
    }

    #[test]
    fn ignores_grouped_self_product_imports() {
        let findings = detect_vacuous_tests(
            "tests/core/wiring_test.rs",
            r#"
use crate::core::target::{self, run_target};

#[test]
fn calls_product() {
    let value = run_target();
    assert_eq!(value, 1);
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_product_imports_exercised_by_test_body() {
        let findings = detect_vacuous_tests(
            "tests/core/wiring_test.rs",
            r#"
use crate::core::target::run_target;

#[test]
fn calls_product() {
    let value = run_target();
    assert_eq!(value, 1);
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_product_imports_exercised_through_helpers() {
        let findings = detect_vacuous_tests(
            "tests/core/extension/bench/runs_flag_test.rs",
            r#"
use crate::core::extension::bench::test_support::{results_with_scenarios, scenario_with_iterations};

fn scenario(id: &str) -> BenchScenario {
    scenario_with_iterations(id, &[("ms", 1.0)], 1)
}

fn results(scenarios: Vec<BenchScenario>) -> BenchResults {
    results_with_scenarios("bench-noop", 5, scenarios)
}

#[test]
fn test_aggregate_runs() {
    let aggregated = aggregate_runs(&[results(vec![scenario("cold")])]).unwrap();
    assert_eq!(aggregated.scenarios.len(), 1);
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_super_grouped_product_calls() {
        let findings = detect_vacuous_tests(
            "tests/core/rig/stack_test.rs",
            r#"
use super::{plan_stack_sync, validate_component_stack_path};

#[test]
fn test_plan_stack_sync_uses_components_with_stack_ids_in_sorted_order() {
    let plan = plan_stack_sync(&rig);
    assert_eq!(plan.len(), 2);
}

#[test]
fn test_validate_component_stack_path_accepts_matching_paths() {
    validate_component_stack_path(&rig, "studio", &stack).expect("path should match");
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn keeps_aliased_grouped_product_import_calls() {
        let findings = detect_vacuous_tests(
            "tests/self_checks_test.rs",
            r#"
use homeboy::commands::lint::{run as run_lint, LintArgs};
use homeboy::commands::test::{run as run_test, TestArgs};

#[test]
fn lint_runs_declared_self_check_without_extensions() {
    let (output, exit_code) = run_lint(lint_args(dir.path()), &GlobalArgs {}).unwrap();
    assert_eq!(exit_code, 0);
    assert!(output.passed);
}

#[test]
fn test_runs_declared_self_check_without_extensions() {
    let (output, exit_code) = run_test(test_args(dir.path()), &GlobalArgs {}).unwrap();
    assert_eq!(exit_code, 0);
    assert!(output.passed);
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
