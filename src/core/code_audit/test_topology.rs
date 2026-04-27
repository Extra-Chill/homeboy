//! Test topology audit — extension-driven test placement policy checks.
//!
//! Core remains language-agnostic. Extensions provide topology signals via
//! `scripts.topology`, and this module enforces repository policy using
//! `audit_rules.test_topology` configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::extension::{self, ExtensionManifest};

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct AuditRulesConfig {
    #[serde(default)]
    pub test_topology: Option<TestTopologyRules>,
}

#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct TestTopologyRules {
    #[serde(default)]
    pub enabled: bool,
    /// Canonical test root(s), usually `tests/**`.
    #[serde(default)]
    pub central_test_globs: Vec<String>,
    /// Optional allowlist for artifacts intentionally kept outside central roots.
    #[serde(default)]
    pub scattered_allow: Vec<String>,
    /// Optional allowlist for source files that may contain inline tests.
    #[serde(default)]
    pub inline_allow: Vec<String>,
    /// Severity for topology findings: "warning" (default) or "info".
    #[serde(default)]
    pub severity: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TopologyInput {
    file_path: String,
    content: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TopologyOutput {
    #[serde(default)]
    artifacts: Vec<TopologyArtifact>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct TopologyArtifact {
    /// Relative path under component root.
    path: String,
    /// "source" | "test" | other extension-defined tags.
    kind: String,
    /// Optional test shape hint (e.g., "inline", "file").
    #[serde(default)]
    shape: Option<String>,
}

pub(super) fn run(root: &Path) -> Vec<Finding> {
    let mut findings = analyze_test_topology(root);
    findings.extend(analyze_test_quality(root));
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn analyze_test_topology(root: &Path) -> Vec<Finding> {
    let rules = load_rules(root).unwrap_or_default();
    if !rules.enabled {
        return Vec::new();
    }

    let central_test_globs = if rules.central_test_globs.is_empty() {
        vec!["tests/**".to_string()]
    } else {
        rules.central_test_globs.clone()
    };

    let severity = parse_severity(rules.severity.as_deref());
    let mut findings = Vec::new();

    for extension in extension::load_all_extensions().unwrap_or_default() {
        let Some(script_rel) = extension.topology_script() else {
            continue;
        };

        let files = codebase_scan::walk_files(
            root,
            &ScanConfig {
                // Use extra_skip_dirs so build dirs are skipped at all depths
                // (matching prior flat skip-list behavior)
                extra_skip_dirs: vec![
                    "build".into(),
                    "dist".into(),
                    "target".into(),
                    "cache".into(),
                    "tmp".into(),
                ],
                extensions: ExtensionFilter::All,
                ..Default::default()
            },
        );
        for file in files {
            let rel = match file.strip_prefix(root) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let Ok(content) = std::fs::read_to_string(&file) else {
                continue;
            };

            let input = TopologyInput {
                file_path: rel.clone(),
                content,
            };

            let artifacts = run_topology_script(&extension, script_rel, &input);
            for artifact in artifacts {
                apply_policy(
                    &artifact,
                    &central_test_globs,
                    &rules,
                    &severity,
                    &mut findings,
                );
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
        .dedup_by(|a, b| a.file == b.file && a.kind == b.kind && a.description == b.description);
    findings
}

fn apply_policy(
    artifact: &TopologyArtifact,
    central_test_globs: &[String],
    rules: &TestTopologyRules,
    severity: &Severity,
    findings: &mut Vec<Finding>,
) {
    let path = &artifact.path;
    let in_central_tests = matches_any(path, central_test_globs);

    if artifact.kind == "test" && !in_central_tests && !matches_any(path, &rules.scattered_allow) {
        findings.push(Finding {
            convention: "test_topology".to_string(),
            severity: severity.clone(),
            file: path.clone(),
            description: "Test artifact is outside centralized test directories".to_string(),
            suggestion: "Move test artifact under central_test_globs (default tests/**) or allowlist it in audit_rules.test_topology.scattered_allow".to_string(),
            kind: AuditFinding::ScatteredTestFile,
        });
    }

    if artifact.kind == "source"
        && artifact.shape.as_deref() == Some("inline_test")
        && !matches_any(path, &rules.inline_allow)
    {
        findings.push(Finding {
            convention: "test_topology".to_string(),
            severity: severity.clone(),
            file: path.clone(),
            description: "Source file contains inline tests outside allowlist".to_string(),
            suggestion: "Prefer isolated tests under central_test_globs; if inline tests are intentional, add this file to audit_rules.test_topology.inline_allow".to_string(),
            kind: AuditFinding::InlineTestModule,
        });
    }
}

fn run_topology_script(
    extension: &ExtensionManifest,
    script_rel: &str,
    input: &TopologyInput,
) -> Vec<TopologyArtifact> {
    let Some(extension_path) = extension.extension_path.as_deref() else {
        return Vec::new();
    };
    let script_path = std::path::Path::new(extension_path).join(script_rel);
    if !script_path.exists() {
        return Vec::new();
    }

    // Invoke the script directly so its shebang resolves the interpreter.
    // Wrapping with `sh <script>` bypasses `#!/usr/bin/env bash` and runs
    // under POSIX sh — which breaks scripts using bash-only features. See #1276.
    let output = std::process::Command::new(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let payload = serde_json::to_vec(input).ok()?;
                let _ = stdin.write_all(&payload);
            }
            child.wait_with_output().ok()
        });

    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<TopologyOutput>(&stdout)
        .map(|o| o.artifacts)
        .unwrap_or_default()
}

fn parse_severity(value: Option<&str>) -> Severity {
    match value.unwrap_or("warning").to_lowercase().as_str() {
        "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

fn matches_any(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|g| glob_match::glob_match(g, path))
}

fn load_rules(root: &Path) -> Option<TestTopologyRules> {
    let homeboy_json = root.join("homeboy.json");
    let content = std::fs::read_to_string(homeboy_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let audit_rules = value.get("audit_rules")?.clone();
    let config: AuditRulesConfig = serde_json::from_value(audit_rules).ok()?;
    config.test_topology
}

#[derive(Debug)]
struct TestFunction {
    name: String,
    body: String,
}

#[derive(Debug, Clone)]
struct EnvMutationSite {
    file: String,
    var: String,
    uses_local_guard: bool,
    uses_shared_guard: bool,
}

fn analyze_test_quality(root: &Path) -> Vec<Finding> {
    let files = codebase_scan::walk_files(
        root,
        &ScanConfig {
            extensions: ExtensionFilter::Only(vec!["rs".to_string()]),
            ..Default::default()
        },
    );

    let mut findings = Vec::new();
    let mut env_mutations: BTreeMap<String, Vec<EnvMutationSite>> = BTreeMap::new();

    for file_path in files {
        let relative = match file_path.strip_prefix(root) {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };

        if !crate::code_audit::walker::is_test_path(&relative) {
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

fn detect_vacuous_tests(file: &str, content: &str) -> Vec<Finding> {
    extract_test_functions(content)
        .into_iter()
        .filter_map(|test| vacuous_reason(&test).map(|reason| (test, reason)))
        .map(|(test, reason)| Finding {
            convention: "test_quality".to_string(),
            severity: Severity::Info,
            file: file.to_string(),
            description: format!("Vacuous test `{}`: {}", test.name, reason),
            suggestion:
                "Delete the placeholder or replace it with a behavior test that calls product code"
                    .to_string(),
            kind: AuditFinding::OrphanedTest,
        })
        .collect()
}

fn vacuous_reason(test: &TestFunction) -> Option<&'static str> {
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

    if !contains_assertion(&body) && !contains_product_reference(&body) {
        return Some("body has no assertion and no product-code reference");
    }

    if contains_assertion(&body)
        && !contains_product_reference(&body)
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

fn contains_product_reference(body: &str) -> bool {
    body.contains("homeboy::")
        || body.contains("crate::")
        || body.contains("super::")
        || body.contains("Command::cargo_bin")
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
    fn test_parse_severity() {
        assert!(matches!(parse_severity(Some("warning")), Severity::Warning));
        assert!(matches!(parse_severity(Some("info")), Severity::Info));
        assert!(matches!(parse_severity(None), Severity::Warning));
    }

    #[test]
    fn test_matches_any() {
        let globs = vec!["tests/**".to_string(), "spec/**".to_string()];
        assert!(matches_any("tests/unit/foo_test.rs", &globs));
        assert!(!matches_any("src/foo.rs", &globs));
    }

    #[test]
    fn test_apply_policy_flags_scattered_test() {
        let artifact = TopologyArtifact {
            path: "src/foo_test.rs".to_string(),
            kind: "test".to_string(),
            shape: Some("file".to_string()),
        };
        let rules = TestTopologyRules {
            enabled: true,
            central_test_globs: vec!["tests/**".to_string()],
            scattered_allow: vec![],
            inline_allow: vec![],
            severity: None,
        };
        let mut findings = Vec::new();
        apply_policy(
            &artifact,
            &rules.central_test_globs,
            &rules,
            &Severity::Warning,
            &mut findings,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::ScatteredTestFile);
    }

    #[test]
    fn test_apply_policy() {
        let artifact = TopologyArtifact {
            path: "src/lib.rs".to_string(),
            kind: "source".to_string(),
            shape: Some("inline_test".to_string()),
        };
        let rules = TestTopologyRules {
            enabled: true,
            central_test_globs: vec!["tests/**".to_string()],
            scattered_allow: vec![],
            inline_allow: vec![],
            severity: None,
        };
        let mut findings = Vec::new();
        apply_policy(
            &artifact,
            &rules.central_test_globs,
            &rules,
            &Severity::Warning,
            &mut findings,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::InlineTestModule);
    }

    #[test]
    fn test_load_rules() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
                "audit_rules": {
                    "test_topology": {
                        "enabled": true,
                        "central_test_globs": ["tests/**"]
                    }
                }
            }"#,
        )
        .expect("homeboy.json should be written");

        let rules = load_rules(dir.path()).expect("rules should load");
        assert!(rules.enabled);
        assert_eq!(rules.central_test_globs, vec!["tests/**".to_string()]);
    }

    #[test]
    fn test_run_topology_script() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let script_rel = "topology.sh";
        let script_path = dir.path().join(script_rel);
        std::fs::write(
            &script_path,
            r#"#!/bin/sh
cat <<'JSON'
{"artifacts":[{"path":"src/foo_test.rs","kind":"test","shape":"file"}]}
JSON
"#,
        )
        .expect("script should be written");

        // `run_topology_script` invokes the script directly (not via `sh <script>`)
        // so the shebang resolves the interpreter — see #1276 / commit 343386a0.
        // Direct invocation requires the execute bit on Unix; chmod +x or the
        // spawn fails with EACCES and the assertion below sees 0 artifacts.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)
                .expect("script metadata should be readable")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).expect("script should become executable");
        }

        let extension = ExtensionManifest {
            id: "test-ext".to_string(),
            name: "Test Extension".to_string(),
            version: "0.1.0".to_string(),
            provides: None,
            scripts: Some(crate::extension::ScriptsConfig {
                topology: Some(script_rel.to_string()),
                ..Default::default()
            }),
            icon: None,
            description: None,
            author: None,
            homepage: None,
            source_url: None,
            deploy: None,
            audit: None,
            executable: None,
            platform: None,
            cli: None,
            build: None,
            lint: None,
            test: None,
            bench: None,
            actions: vec![],
            hooks: std::collections::HashMap::new(),
            settings: vec![],
            requires: None,
            autofix_verify: None,
            extra: std::collections::HashMap::new(),
            extension_path: Some(dir.path().to_string_lossy().to_string()),
        };

        let artifacts = run_topology_script(
            &extension,
            script_rel,
            &TopologyInput {
                file_path: "src/lib.rs".to_string(),
                content: "pub fn x(){}".to_string(),
            },
        );

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "src/foo_test.rs");
        assert_eq!(artifacts[0].kind, "test");
    }

    #[test]
    fn test_analyze_test_topology() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        std::fs::write(
            dir.path().join("homeboy.json"),
            r#"{
                "audit_rules": {
                    "test_topology": {
                        "enabled": true,
                        "central_test_globs": ["tests/**"],
                        "scattered_allow": [],
                        "inline_allow": []
                    }
                }
            }"#,
        )
        .expect("homeboy.json should be written");

        // No installed extension topology scripts in unit-test context;
        // analyzer should still execute and return deterministic empty findings.
        let findings = analyze_test_topology(dir.path());
        assert!(findings.is_empty());
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
        assert_eq!(findings[0].kind, AuditFinding::OrphanedTest);
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

    #[test]
    fn test_run() {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let findings = run(dir.path());
        assert!(findings.is_empty());
    }
}
