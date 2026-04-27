//! Detect tests that mutate process-global environment variables without a shared guard.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

static ENV_MUTATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"std::env::(?:set_var|remove_var)\s*\(\s*\"([A-Z_][A-Z0-9_]*)\""#)
        .expect("env mutation regex compiles")
});

#[derive(Debug)]
struct EnvMutationSite<'a> {
    fp: &'a FileFingerprint,
    env_var: String,
    line: usize,
}

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut by_var: HashMap<String, Vec<EnvMutationSite<'_>>> = HashMap::new();
    for fp in fingerprints {
        if fp.language != Language::Rust || !is_test_file(&fp.relative_path) {
            continue;
        }
        for cap in ENV_MUTATION_RE.captures_iter(&fp.content) {
            let full = cap.get(0).unwrap();
            let env_var = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if env_var != "HOME" {
                continue;
            }
            by_var
                .entry(env_var.to_string())
                .or_default()
                .push(EnvMutationSite {
                    fp,
                    env_var: env_var.to_string(),
                    line: line_of_offset(&fp.content, full.start()),
                });
        }
    }

    let mut findings = Vec::new();
    for (env_var, sites) in by_var {
        let mut files: Vec<String> = sites
            .iter()
            .map(|site| site.fp.relative_path.clone())
            .collect();
        files.sort();
        files.dedup();
        if files.len() <= 1 {
            continue;
        }

        let canonical_files: Vec<String> = files
            .iter()
            .filter(|file| is_canonical_env_guard_file(file))
            .cloned()
            .collect();
        for site in sites {
            if is_canonical_env_guard_file(&site.fp.relative_path) {
                continue;
            }
            findings.push(Finding {
                convention: "global_env_guard".to_string(),
                severity: Severity::Warning,
                file: site.fp.relative_path.clone(),
                description: format!(
                    "Test mutates process-global `{}` at line {} while {} file(s) mutate the same env var",
                    site.env_var,
                    site.line,
                    files.len()
                ),
                suggestion: if canonical_files.is_empty() {
                    format!(
                        "Centralize `{}` isolation in one test-support guard with a shared mutex, then import that helper instead of hand-rolling local guards.",
                        env_var
                    )
                } else {
                    format!(
                        "Use the canonical `{}` isolation helper in {} instead of mutating the env var locally.",
                        env_var,
                        canonical_files.join(", ")
                    )
                },
                kind: AuditFinding::GlobalEnvMutationGuard,
            });
        }
    }

    findings
}

fn is_test_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.starts_with("tests/")
        || normalized.contains("/tests/")
        || normalized.ends_with("_test.rs")
        || normalized.ends_with("test_support.rs")
}

fn is_canonical_env_guard_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.ends_with("/support.rs")
        || normalized.contains("/support/")
        || normalized.ends_with("/test_helpers.rs")
        || normalized.ends_with("/test_support.rs")
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

    fn rust_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_run() {
        let support = rust_fp(
            "src/core/rig/test_support.rs",
            r#"pub(crate) fn guard() { std::env::set_var("HOME", "tmp"); }"#,
        );

        assert!(run(&[&support]).is_empty());
    }

    #[test]
    fn flags_multiple_local_home_env_guards_across_test_files() {
        let runner = rust_fp(
            "tests/core/rig/runner_test.rs",
            r#"
fn home_lock() {}
fn with_isolated_home() {
    std::env::set_var("HOME", "tmp");
}
"#,
        );
        let install = rust_fp(
            "tests/core/rig/install_test.rs",
            r#"
struct HomeGuard;
impl HomeGuard {
    fn new() { std::env::set_var("HOME", "tmp"); }
}
"#,
        );

        let findings = run(&[&runner, &install]);
        assert_eq!(findings.len(), 2);
        assert!(findings
            .iter()
            .all(|finding| finding.kind == AuditFinding::GlobalEnvMutationGuard));
        assert!(findings
            .iter()
            .any(|finding| finding.file == runner.relative_path));
        assert!(findings
            .iter()
            .any(|finding| finding.file == install.relative_path));
        assert!(findings[0]
            .suggestion
            .contains("Centralize `HOME` isolation"));
    }

    #[test]
    fn ignores_single_canonical_home_guard_file() {
        let support = rust_fp(
            "src/core/rig/test_support.rs",
            r#"
pub(crate) struct HomeGuard;
impl HomeGuard {
    pub(crate) fn new() { std::env::set_var("HOME", "tmp"); }
}
impl Drop for HomeGuard {
    fn drop(&mut self) { std::env::remove_var("HOME"); }
}
"#,
        );
        let runner = rust_fp(
            "tests/core/rig/runner_test.rs",
            r#"
use crate::rig::test_support::with_isolated_home;
"#,
        );

        assert!(run(&[&support, &runner]).is_empty());
    }

    #[test]
    fn points_noncanonical_mutation_at_existing_support_guard() {
        let support = rust_fp(
            "src/core/rig/test_support.rs",
            r#"pub(crate) fn guard() { std::env::set_var("HOME", "tmp"); }"#,
        );
        let local = rust_fp(
            "tests/core/rig/install_test.rs",
            r#"fn test() { std::env::set_var("HOME", "tmp"); }"#,
        );

        let findings = run(&[&support, &local]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, local.relative_path);
        assert!(findings[0]
            .suggestion
            .contains("src/core/rig/test_support.rs"));
    }
}
