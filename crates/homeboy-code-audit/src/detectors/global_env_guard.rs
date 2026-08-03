//! Detect tests that mutate process-global environment variables without a shared guard.
//!
//! Rust runs a crate's tests as threads in one process, so a mutation of a
//! process-global variable is visible to every other test in the binary. Two
//! shapes are reported:
//!
//! - **Unrestored**: a test function alters a variable and never puts it back.
//!   This is a bug on its own, independent of how many files are involved —
//!   homeboy #11348 deleted `HOME` this way and broke ~90 downstream tests for
//!   twelve days.
//! - **Uncoordinated**: several files mutate the same variable with no shared
//!   isolation helper, so their ordering decides the outcome.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::source_locations::line_of_offset;
use super::source_text::SourceMasks;

static ENV_MUTATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"std::env::(?:set_var|remove_var)\s*\(\s*\"([A-Z_][A-Z0-9_]*)\""#)
        .expect("env mutation regex compiles")
});

/// Reads that capture a variable so it can be put back afterwards.
static ENV_CAPTURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"std::env::var(?:_os)?\s*\(\s*(?:\"([A-Z_][A-Z0-9_]*)\"|&?[a-z_][a-z0-9_]*)"#)
        .expect("env capture regex compiles")
});

/// Calls that hand isolation to a shared, process-wide helper.
const CANONICAL_GUARD_CALLS: [&str; 4] = [
    "with_isolated_home",
    "with_isolated_audit_home",
    "env_lock()",
    "home_env_guard()",
];

#[derive(Debug)]
struct EnvMutationSite<'a> {
    fp: &'a FileFingerprint,
    env_var: String,
    line: usize,
    /// Whether the enclosing function shows any sign of putting the variable
    /// back — a captured prior value, a `Drop` restore, or a shared helper.
    restored: bool,
    /// Whether the mutation is ordered against the rest of the binary by the
    /// shared isolation helper. A correct but hand-rolled guard restores its
    /// own value yet still races every other file that touches the variable.
    coordinated: bool,
}

pub(crate) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut by_var: HashMap<String, Vec<EnvMutationSite<'_>>> = HashMap::new();
    for fp in fingerprints {
        if fp.language != Language::Rust {
            continue;
        }
        let Some(region) = test_region(&fp.relative_path, &fp.content) else {
            continue;
        };
        let masks = SourceMasks::new(&fp.content, fp.language);
        for cap in ENV_MUTATION_RE.captures_iter(&fp.content[region.clone()]) {
            let full = cap.get(0).unwrap();
            let offset = region.start + full.start();
            let env_var = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            if env_var != "HOME" {
                continue;
            }
            // A detector's own fixtures embed sample source in string literals.
            // Real code has `std::env::` in code position and only the variable
            // name quoted; fixture text has the whole statement inside a string.
            if is_inside_string_literal(&fp.content, &masks, offset) {
                continue;
            }
            by_var
                .entry(env_var.to_string())
                .or_default()
                .push(EnvMutationSite {
                    fp,
                    env_var: env_var.to_string(),
                    line: line_of_offset(&fp.content, offset),
                    restored: mutation_is_restored(&fp.content, offset, env_var),
                    coordinated: mutation_is_coordinated(&fp.content, offset),
                });
        }
    }

    let mut findings = Vec::new();

    // An unrestored mutation is a bug by itself. Report it before the coupling
    // rule, which only fires when several files are involved and so cannot see
    // a single test that deletes a variable and walks away.
    for (env_var, sites) in &by_var {
        let canonical = canonical_guard_files(sites);
        for site in sites.iter().filter(|site| !site.restored) {
            if is_canonical_env_guard_file(&site.fp.relative_path) {
                continue;
            }
            findings.push(Finding {
                convention: "global_env_guard".to_string(),
                severity: Severity::Warning,
                file: site.fp.relative_path.clone(),
                description: format!(
                    "Test mutates process-global `{}` at line {} without restoring it",
                    site.env_var, site.line
                ),
                // Where a canonical helper already exists, naming it is more
                // actionable than describing the guard the author should write.
                suggestion: if canonical.is_empty() {
                    format!(
                        "Capture the prior value and restore it from a `Drop` guard so `{}` survives this test, including when an assertion panics before any manual restore.",
                        env_var
                    )
                } else {
                    format!(
                        "Use the canonical `{}` isolation helper in {} instead of mutating the env var locally; it restores the prior value even when a test panics.",
                        env_var,
                        canonical.join(", ")
                    )
                },
                kind: AuditFinding::GlobalEnvMutationGuard,
                line: Some(site.line as u32),
            });
        }
    }

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

        let canonical_files = canonical_guard_files(&sites);
        for site in sites {
            if is_canonical_env_guard_file(&site.fp.relative_path) {
                continue;
            }
            // Already reported, more precisely, as an unrestored mutation. One
            // problem earns one finding.
            if !site.restored {
                continue;
            }
            // Deferring to the shared helper is the fix this rule asks for.
            if site.coordinated {
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
                            line: Some(site.line as u32),
            });
        }
    }

    findings
}

/// Whether `offset` falls inside a string literal.
fn is_inside_string_literal(content: &str, masks: &SourceMasks, offset: usize) -> bool {
    let line_start = content[..offset]
        .rfind('\n')
        .map(|newline| newline + 1)
        .unwrap_or(0);
    let column = content[line_start..offset].chars().count();
    masks
        .strings(line_of_offset(content, offset) - 1)
        .chars()
        .nth(column)
        .is_some_and(|ch| !ch.is_whitespace())
}

/// Files among `sites` that hold a canonical, reusable isolation helper.
fn canonical_guard_files(sites: &[EnvMutationSite<'_>]) -> Vec<String> {
    let mut files: Vec<String> = sites
        .iter()
        .map(|site| site.fp.relative_path.clone())
        .filter(|file| is_canonical_env_guard_file(file))
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Whether the function containing `offset` shows evidence of restoring `env_var`.
///
/// Scoped to the enclosing function rather than the file. File-level scanning
/// is what let #11348 hide: `agent_task_gate.rs` had a correct save/restore in
/// one test, which vouched for a second test in the same file that deleted
/// `HOME` outright.
fn mutation_is_restored(content: &str, offset: usize, env_var: &str) -> bool {
    // A mutation inside `Drop::drop` is the restoring half of an RAII guard —
    // the pattern this detector wants people to adopt, not a hazard.
    if enclosing_function_name(content, offset) == Some("drop") {
        return true;
    }
    if mutation_is_coordinated(content, offset) {
        return true;
    }
    let body = enclosing_function(content, offset);
    ENV_CAPTURE_RE.captures_iter(body).any(|cap| {
        // A capture keyed by a runtime name (`var_os(name)`, as an RAII guard
        // does) cannot be attributed to one variable, so it counts for all.
        cap.get(1).is_none_or(|literal| literal.as_str() == env_var)
    })
}

/// Name of the function enclosing `offset`.
fn enclosing_function_name(content: &str, offset: usize) -> Option<&str> {
    static FN_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)").expect("fn name regex compiles")
    });
    FN_NAME_RE
        .captures_iter(&content[..offset])
        .last()
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str())
}

/// Whether the function enclosing `offset` defers isolation to the shared
/// helper, which orders it against every other test in the binary.
fn mutation_is_coordinated(content: &str, offset: usize) -> bool {
    let body = enclosing_function(content, offset);
    CANONICAL_GUARD_CALLS.iter().any(|call| body.contains(call))
}

/// Body of the function enclosing `offset`, as a best-effort span running from
/// the opening brace to the next `fn` keyword.
///
/// The signature is deliberately excluded: a function *named*
/// `with_isolated_home` must not be read as one that *calls* it.
fn enclosing_function(content: &str, offset: usize) -> &str {
    static FN_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\bfn\s+[A-Za-z_]").expect("fn regex compiles"));
    let signature = FN_RE
        .find_iter(&content[..offset])
        .last()
        .map(|m| m.start())
        .unwrap_or(0);
    let start = content[signature..offset]
        .find('{')
        .map(|brace| signature + brace + 1)
        .unwrap_or(signature);
    let end = FN_RE
        .find(&content[offset..])
        .map(|m| offset + m.start())
        .unwrap_or(content.len());
    &content[start..end]
}

/// Byte range of `content` that holds test code, or `None` when there is none.
///
/// A path-named test file is test code end to end. Everything else is scanned
/// from its first `#[cfg(test)]` marker, which is where Rust's dominant unit
/// test idiom lives — an inline `mod tests` at the bottom of a production
/// source file.
///
/// Path heuristics alone missed that idiom entirely. `agent_task_gate.rs`
/// deleted `HOME` for the whole test process from inside an inline
/// `#[cfg(test)] mod tests`, and this detector never saw the file (#11349).
fn test_region(path: &str, content: &str) -> Option<std::ops::Range<usize>> {
    if is_test_path(path) {
        return Some(0..content.len());
    }
    content
        .find(INLINE_TEST_MARKER)
        .map(|start| start..content.len())
}

/// Attribute that opens an inline test module or item.
const INLINE_TEST_MARKER: &str = "#[cfg(test)]";

fn is_test_path(path: &str) -> bool {
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
        // Neither site restores what it changed, so each is reported for that
        // rather than for the weaker "several files touch this" signal.
        assert!(findings
            .iter()
            .all(|finding| finding.description.contains("without restoring it")));
    }

    #[test]
    fn flags_an_unrestored_mutation_inside_an_inline_test_module() {
        // homeboy #11348 verbatim: an inline `#[cfg(test)] mod tests` in a
        // production source file, setting HOME and then deleting it rather than
        // putting back what was there. Path heuristics never saw this file.
        let gate = rust_fp(
            "crates/homeboy-agents/src/agent_task_gate.rs",
            r#"
pub fn run_gate_command() {}

#[cfg(test)]
mod tests {
    #[test]
    fn isolated_gate_does_not_observe_ambient_state() {
        std::env::set_var("HOME", &home);
        let report = run_gate_command();
        std::env::remove_var("HOME");
    }
}
"#,
        );

        let findings = run(&[&gate]);

        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings
            .iter()
            .all(|finding| finding.file == gate.relative_path));
        assert!(findings
            .iter()
            .all(|finding| finding.description.contains("without restoring it")));
    }

    #[test]
    fn ignores_production_env_mutation_outside_the_test_region() {
        // Scanning from the `#[cfg(test)]` marker keeps production code out of
        // a test-hazard detector.
        let source = rust_fp(
            "crates/homeboy-core/src/thing.rs",
            r#"
pub fn adopt_home(path: &str) {
    std::env::set_var("HOME", path);
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_only() {
        assert!(std::env::var_os("HOME").is_some());
    }
}
"#,
        );

        assert!(run(&[&source]).is_empty(), "{:?}", run(&[&source]));
    }

    #[test]
    fn a_file_without_tests_is_not_scanned() {
        let source = rust_fp(
            "crates/homeboy-core/src/thing.rs",
            r#"pub fn adopt_home(path: &str) { std::env::set_var("HOME", path); }"#,
        );

        assert!(run(&[&source]).is_empty());
    }

    #[test]
    fn a_saved_and_restored_mutation_is_not_flagged_as_unrestored() {
        let restoring = rust_fp(
            "tests/core/restoring_test.rs",
            r#"
fn toolchain_preflight_reports_declared_homes() {
    let prior_home = std::env::var_os("HOME");
    std::env::set_var("HOME", original_home.path());
    let result = preflight();
    match prior_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    result.expect("declared cargo homes initialize the toolchain");
}
"#,
        );

        assert!(run(&[&restoring]).is_empty(), "{:?}", run(&[&restoring]));
    }

    #[test]
    fn a_shared_isolation_helper_counts_as_restoration() {
        let guarded = rust_fp(
            "tests/core/guarded_test.rs",
            r#"
fn uses_the_shared_helper() {
    let _lock = homeboy_core::test_support::env_lock();
    std::env::set_var("HOME", scratch.path());
}
"#,
        );

        assert!(run(&[&guarded]).is_empty(), "{:?}", run(&[&guarded]));
    }

    #[test]
    fn a_function_named_like_the_helper_does_not_vouch_for_itself() {
        // The signature is excluded from the searched body, so declaring
        // `fn with_isolated_home()` is not the same as calling it.
        let impostor = rust_fp(
            "tests/core/impostor_test.rs",
            r#"
fn with_isolated_home() {
    std::env::set_var("HOME", "tmp");
}
"#,
        );

        let findings = run(&[&impostor]);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].description.contains("without restoring it"));
    }

    #[test]
    fn a_drop_impl_is_the_restoring_half_of_a_guard() {
        // `EnvironmentGuard` in crates/homeboy-agents/tests/durable_promotion.rs
        // is a correct RAII guard. The old rule reported it anyway (#11305),
        // while missing the test that actually deleted HOME.
        let guard = rust_fp(
            "tests/core/guard_test.rs",
            r#"
impl EnvironmentGuard {
    fn isolate(path: &std::path::Path) -> Self {
        let guard = Self { home: std::env::var_os("HOME") };
        std::env::set_var("HOME", path);
        guard
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match &self.home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}
"#,
        );

        let findings = run(&[&guard]);
        assert!(
            findings
                .iter()
                .all(|finding| !finding.description.contains("without restoring it")),
            "{findings:?}"
        );
    }

    #[test]
    fn the_shared_helper_also_satisfies_the_coupling_rule() {
        // Using the process-wide helper is the fix the coupling rule asks for,
        // so it must not keep reporting a test that already adopted it.
        let coordinated = rust_fp(
            "tests/reverse_cook_queue_acceptance.rs",
            r#"
fn pinned_runner_route_persists_the_outcome() {
    let _env_guard = homeboy_core::test_support::home_env_guard();
    std::env::set_var("HOME", context.home());
}
"#,
        );
        let hand_rolled = rust_fp(
            "tests/core/hand_rolled_test.rs",
            r#"
fn isolate() {
    let prior = std::env::var_os("HOME");
    std::env::set_var("HOME", "tmp");
}
"#,
        );

        let findings = run(&[&coordinated, &hand_rolled]);

        assert!(
            findings
                .iter()
                .all(|finding| finding.file == hand_rolled.relative_path),
            "the helper-using test must be clean: {findings:?}"
        );
    }

    #[test]
    fn fixture_source_inside_a_string_literal_is_not_a_mutation() {
        // A detector's own test fixtures embed sample source in string
        // literals. Reading them as real mutations makes the detector report
        // itself.
        let detector = rust_fp(
            "crates/homeboy-code-audit/src/detectors/global_env_guard.rs",
            "#[cfg(test)]\nmod tests {\n    fn fixture() {\n        let sample = r#\"fn t() { std::env::set_var(\"HOME\", \"/tmp\"); }\"#;\n    }\n}\n",
        );

        assert!(run(&[&detector]).is_empty(), "{:?}", run(&[&detector]));
    }

    #[test]
    fn uncoordinated_but_restored_mutations_still_report_the_coupling() {
        // Both restore correctly, so the only remaining hazard is that their
        // ordering is unsynchronized across files.
        let one = rust_fp(
            "tests/core/one_test.rs",
            r#"
fn a() {
    let prior = std::env::var_os("HOME");
    std::env::set_var("HOME", "one");
}
"#,
        );
        let two = rust_fp(
            "tests/core/two_test.rs",
            r#"
fn b() {
    let prior = std::env::var_os("HOME");
    std::env::set_var("HOME", "two");
}
"#,
        );

        let findings = run(&[&one, &two]);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings.iter().all(|finding| finding
            .description
            .contains("file(s) mutate the same env var")));
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
    pub fn new() { std::env::set_var("HOME", "tmp"); }
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
