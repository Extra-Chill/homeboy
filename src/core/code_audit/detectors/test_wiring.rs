use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use super::conventions::unwired_test_file_finding;
use super::findings::{Finding, Severity};
use crate::core::component::{AuditConfig, TestWiringPolicy};

pub(crate) fn run(root: &Path, audit_config: &AuditConfig) -> Vec<Finding> {
    let mut findings = Vec::new();

    for policy in &audit_config.test_wiring.policies {
        if !policy.require_explicit_wiring {
            continue;
        }

        let source_text = collect_source_text(root, &policy.source_path_globs);
        for relative in collect_matching_files(root, &policy.test_path_globs) {
            if !requires_wiring(&relative, policy) || is_wired(&relative, &source_text, policy) {
                continue;
            }

            findings.push(Finding {
                convention: policy.convention.clone(),
                severity: parse_severity(&policy.severity),
                file: relative.clone(),
                description: render_template(&policy.description, &relative),
                suggestion: render_template(&policy.suggestion, &relative),
                kind: unwired_test_file_finding(),
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
        .dedup_by(|a, b| a.file == b.file && a.kind == b.kind && a.description == b.description);
    findings
}

fn requires_wiring(relative: &str, policy: &TestWiringPolicy) -> bool {
    matches_any(relative, &policy.test_path_globs)
        && !matches_any(relative, &policy.auto_discovered_test_path_globs)
        && !matches_any(relative, &policy.support_test_path_globs)
}

fn is_wired(relative: &str, source_text: &str, policy: &TestWiringPolicy) -> bool {
    policy
        .explicit_wiring_marker_patterns
        .iter()
        .any(|pattern| marker_matches(pattern, relative, source_text))
}

fn marker_matches(pattern: &str, relative: &str, source_text: &str) -> bool {
    let pattern = pattern.replace("{test_path}", &regex::escape(relative));
    Regex::new(&pattern)
        .ok()
        .is_some_and(|regex| regex.is_match(source_text))
}

fn collect_source_text(root: &Path, source_path_globs: &[String]) -> String {
    let mut text = String::new();
    for relative in collect_matching_files(root, source_path_globs) {
        if let Ok(content) = fs::read_to_string(root.join(&relative)) {
            text.push_str(&content);
            text.push('\n');
        }
    }
    text
}

fn collect_matching_files(root: &Path, globs: &[String]) -> Vec<String> {
    let mut files = Vec::new();
    let mut roots = scan_roots(root, globs);
    roots.sort();
    roots.dedup();

    for scan_root in roots {
        for path in collect_files(&scan_root) {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = normalize_path(relative);
            if matches_any(&relative, globs) && !files.contains(&relative) {
                files.push(relative);
            }
        }
    }

    files.sort();
    files
}

fn scan_roots(root: &Path, globs: &[String]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for glob in globs {
        let prefix = glob
            .split(['*', '?', '['])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        let prefix = prefix
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(prefix);
        roots.push(if prefix.is_empty() {
            root.to_path_buf()
        } else {
            root.join(prefix)
        });
    }
    if roots.is_empty() {
        roots.push(root.to_path_buf());
    }
    roots
}

fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_into(dir, &mut files);
    files.sort();
    files
}

fn collect_files_into(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_into(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn matches_any(path: &str, globs: &[String]) -> bool {
    globs.iter().any(|glob| glob_match::glob_match(glob, path))
}

fn parse_severity(value: &str) -> Severity {
    match value.to_ascii_lowercase().as_str() {
        "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

fn render_template(template: &str, test_path: &str) -> String {
    template.replace("{test_path}", test_path)
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::TestWiringConfig;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write fixture");
    }

    fn policy() -> AuditConfig {
        AuditConfig {
            test_wiring: TestWiringConfig {
                policies: vec![TestWiringPolicy {
                    id: "fixture".to_string(),
                    source_path_globs: vec!["source/**/*.code".to_string()],
                    test_path_globs: vec!["checks/**/*.case".to_string()],
                    auto_discovered_test_path_globs: vec!["checks/*.case".to_string()],
                    support_test_path_globs: vec!["checks/**/support.case".to_string()],
                    require_explicit_wiring: true,
                    explicit_wiring_marker_patterns: vec![
                        "include\\(\"{test_path}\"\\)".to_string()
                    ],
                    convention: "configured wiring".to_string(),
                    severity: "warning".to_string(),
                    description: "`{test_path}` needs wiring".to_string(),
                    suggestion: "Add include for `{test_path}`".to_string(),
                }],
            },
            ..Default::default()
        }
    }

    fn nested_repository_policy() -> AuditConfig {
        AuditConfig {
            test_wiring: TestWiringConfig {
                policies: vec![TestWiringPolicy {
                    id: "nested".to_string(),
                    source_path_globs: vec!["src/**/*.rs".to_string()],
                    test_path_globs: vec!["tests/**/*_test.rs".to_string()],
                    auto_discovered_test_path_globs: vec!["tests/*_test.rs".to_string()],
                    support_test_path_globs: Vec::new(),
                    require_explicit_wiring: true,
                    explicit_wiring_marker_patterns: vec!["{test_path}".to_string()],
                    convention: "test_wiring".to_string(),
                    severity: "warning".to_string(),
                    description: "`{test_path}` needs wiring".to_string(),
                    suggestion: "Add wiring for `{test_path}`".to_string(),
                }],
            },
            ..Default::default()
        }
    }

    #[test]
    fn flags_configured_test_file_without_source_marker() {
        let dir = TempDir::new().expect("tempdir");
        write(&dir.path().join("source/unit/item.code"), "fn item() {}\n");
        write(&dir.path().join("checks/unit/item.case"), "check item\n");

        let findings = run(dir.path(), &policy());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "checks/unit/item.case");
        assert_eq!(findings[0].convention, "configured wiring");
        assert_eq!(
            findings[0].description,
            "`checks/unit/item.case` needs wiring"
        );
    }

    #[test]
    fn accepts_configured_test_file_with_source_marker() {
        let dir = TempDir::new().expect("tempdir");
        write(
            &dir.path().join("source/unit/item.code"),
            "include(\"checks/unit/item.case\")\n",
        );
        write(&dir.path().join("checks/unit/item.case"), "check item\n");

        let findings = run(dir.path(), &policy());

        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_auto_discovered_and_support_tests() {
        let dir = TempDir::new().expect("tempdir");
        write(&dir.path().join("source/unit/item.code"), "fn item() {}\n");
        write(&dir.path().join("checks/api.case"), "check api\n");
        write(&dir.path().join("checks/unit/support.case"), "helper\n");

        let findings = run(dir.path(), &policy());

        assert!(findings.is_empty());
    }

    #[test]
    fn preserves_nested_repository_test_path_policy() {
        let dir = TempDir::new().expect("tempdir");
        write(&dir.path().join("src/core/foo.rs"), "pub fn foo() {}\n");
        write(
            &dir.path().join("tests/core/foo_test.rs"),
            "#[test] fn works() {}\n",
        );
        write(
            &dir.path().join("tests/api_jobs_test.rs"),
            "#[test] fn works() {}\n",
        );

        let findings = run(dir.path(), &nested_repository_policy());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "tests/core/foo_test.rs");
    }

    #[test]
    fn accepts_nested_repository_test_path_when_source_references_it() {
        let dir = TempDir::new().expect("tempdir");
        write(
            &dir.path().join("src/core/foo.rs"),
            "#[path = \"../../tests/core/foo_test.rs\"]\nmod foo_test;\n",
        );
        write(
            &dir.path().join("tests/core/foo_test.rs"),
            "#[test] fn works() {}\n",
        );

        let findings = run(dir.path(), &nested_repository_policy());

        assert!(findings.is_empty());
    }
}
