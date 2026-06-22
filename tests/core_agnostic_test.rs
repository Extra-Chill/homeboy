use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy)]
struct Term {
    name: &'static str,
    kind: MatchKind,
}

#[derive(Clone, Copy)]
enum MatchKind {
    Literal,
    Token,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct ViolationKey {
    path: &'static str,
    term: &'static str,
}

const CORE_OWNED_SOURCE_ROOTS: &[&str] = &[
    "src/core",
    "src/commands/component.rs",
    "src/commands/doctor/resources.rs",
    "src/commands/extension.rs",
    "src/commands/lint.rs",
    "src/commands/report.rs",
    "src/commands/review/mod.rs",
    "src/commands/test.rs",
];

const CORE_OWNED_TEST_CONTENT_ROOTS: &[&str] = &["tests/core", "tests/fixtures"];

const CORE_EXTENSION_BOUNDARY_ROOTS: &[&str] =
    &["src/core", "src/commands", "tests/core", "tests/fixtures"];

// Concrete extension IDs belong in extension-owned fixtures/config, not in
// Homeboy core or core-owned tests. Keep this list to IDs provided by external
// extensions so the guard stays independent of any one regression site.
const CONCRETE_EXTENSION_IDS: &[Term] = &[Term {
    name: "nodejs",
    kind: MatchKind::Token,
}];

const TERMS: &[Term] = &[
    Term {
        name: "wordpress",
        kind: MatchKind::Token,
    },
    Term {
        name: "nodejs",
        kind: MatchKind::Token,
    },
    Term {
        name: "rust",
        kind: MatchKind::Token,
    },
    Term {
        name: "php",
        kind: MatchKind::Token,
    },
    Term {
        name: "cargo",
        kind: MatchKind::Token,
    },
    Term {
        name: "npm",
        kind: MatchKind::Token,
    },
    Term {
        name: "npx",
        kind: MatchKind::Token,
    },
    Term {
        name: "composer",
        kind: MatchKind::Token,
    },
    Term {
        name: "phpcbf",
        kind: MatchKind::Token,
    },
    Term {
        name: "phpcs",
        kind: MatchKind::Token,
    },
    Term {
        name: "phpstan",
        kind: MatchKind::Token,
    },
    Term {
        name: "gofmt",
        kind: MatchKind::Token,
    },
    Term {
        name: "Cargo.toml",
        kind: MatchKind::Literal,
    },
    Term {
        name: "Cargo.lock",
        kind: MatchKind::Literal,
    },
    Term {
        name: "package.json",
        kind: MatchKind::Literal,
    },
    Term {
        name: "composer.json",
        kind: MatchKind::Literal,
    },
    Term {
        name: "tsconfig.json",
        kind: MatchKind::Literal,
    },
    Term {
        name: "tsbuildinfo",
        kind: MatchKind::Token,
    },
    Term {
        name: "go vet",
        kind: MatchKind::Literal,
    },
    Term {
        name: "wp-content",
        kind: MatchKind::Literal,
    },
    Term {
        name: "style.css",
        kind: MatchKind::Literal,
    },
    Term {
        name: "functions.php",
        kind: MatchKind::Literal,
    },
    Term {
        name: "WP_CLI",
        kind: MatchKind::Literal,
    },
    Term {
        name: "WooCommerce",
        kind: MatchKind::Literal,
    },
    Term {
        name: "Action Scheduler",
        kind: MatchKind::Literal,
    },
    Term {
        name: "Homeboy",
        kind: MatchKind::Token,
    },
    Term {
        name: "homeboy.json",
        kind: MatchKind::Literal,
    },
    Term {
        name: ".homeboy",
        kind: MatchKind::Literal,
    },
    Term {
        name: "HOMEBOY_",
        kind: MatchKind::Literal,
    },
    Term {
        name: "homeboy/lab-offload/v1",
        kind: MatchKind::Literal,
    },
    Term {
        name: "Lab",
        kind: MatchKind::Token,
    },
    Term {
        name: "offload",
        kind: MatchKind::Token,
    },
    Term {
        name: "homeboy-run",
        kind: MatchKind::Literal,
    },
    Term {
        name: "runner-artifact://",
        kind: MatchKind::Literal,
    },
];

// Known core-owned test/fixture literal debt tracked by #3034. Keep this list
// explicit so stale rows and occurrence-count changes force cleanup or review.
const TEST_CONTENT_BASELINE: &[ViolationKey] = &[
    ViolationKey {
        path: "src/core/code_audit/core_fingerprint.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/repeated_literal_shape.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/test_quality.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/context/mod.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/context/mod.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/engine/symbol_graph.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/bench/run_metadata.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/extension/registry.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/extension/summary.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/extension/test/parsing.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/extension/test/report.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/git/commits.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/observation/budget_findings.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/observation/records/run_builder.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/refactor/decompose.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/refactor/decompose.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/release/planning_worktree.rs",
        term: "npm",
    },
    ViolationKey {
        path: "src/core/rig/spec.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/runner/mod.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/server/health.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/triage/tests.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "tests/core/daemon_test.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "tests/core/extension/component_script_test.rs",
        term: "php",
    },
    ViolationKey {
        path: "tests/core/rig/bench_default_baseline_dispatch_test.rs",
        term: "rust",
    },
    ViolationKey {
        path: "tests/core/rig/spec_test.rs",
        term: "php",
    },
    ViolationKey {
        path: "tests/core/rig/spec_test.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "tests/fixtures/failure_digest/lint.json",
        term: "phpcs",
    },
    ViolationKey {
        path: "tests/fixtures/failure_digest/lint.json",
        term: "phpstan",
    },
];

const TEST_CONTENT_BASELINE_OCCURRENCES: usize = 79;
const CORE_AGNOSTIC_REPAIR_DIRECTIVE: &str = "This is a boundary violation, not a baseline chore. Do not add these findings to the baseline unless explicitly approved by a maintainer. Move platform-specific behavior into the owning extension or replace it with a generic core contract that extensions can populate.";

#[test]
fn core_owned_source_stays_language_and_framework_agnostic() {
    if release_ci_tracks_audit_without_blocking() {
        eprintln!(
            "skipping release-blocking core-boundary assertion because audit is tracked outside the release-blocking command set"
        );
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let findings = homeboy::core::code_audit::source_policy_findings_for_path(
        "homeboy",
        root.to_str().expect("manifest dir is UTF-8"),
    )
    .expect("source policy should run");
    let baseline = homeboy::core::code_audit::baseline::load_baseline(root)
        .expect("homeboy.json should contain an audit baseline");

    let policy = homeboy::core::code_audit::baseline::SOURCE_POLICY_CORE_BOUNDARY_POLICY;
    let scope = Some(homeboy::core::code_audit::baseline::SOURCE_POLICY_CORE_BOUNDARY_SCOPE);
    let current_policy_findings = findings
        .iter()
        .filter(|finding| finding.convention == policy)
        .filter(|finding| source_policy_finding_in_scope(root, &finding.file))
        .collect::<Vec<_>>();

    let baseline_fingerprints =
        homeboy::core::code_audit::baseline::policy_baseline_fingerprints(&baseline, policy, scope);
    let current_policy_fingerprints = current_policy_findings
        .iter()
        .map(|finding| homeboy::core::code_audit::baseline::finding_baseline_fingerprint(finding))
        .collect::<BTreeSet<_>>();

    let baseline_mode = if is_changed_scope_run() {
        homeboy::core::code_audit::baseline::PolicyBaselineMode::ChangedScope
    } else {
        homeboy::core::code_audit::baseline::PolicyBaselineMode::Full
    };
    let new_policy_findings =
        if baseline_mode == homeboy::core::code_audit::baseline::PolicyBaselineMode::ChangedScope {
            Vec::new()
        } else {
            current_policy_findings
                .iter()
                .filter_map(|finding| {
                    let fingerprint =
                        homeboy::core::code_audit::baseline::finding_baseline_fingerprint(finding);
                    (!baseline_fingerprints.contains(fingerprint.as_str()))
                        .then(|| format!("{}: {}", fingerprint, finding.description))
                })
                .collect::<Vec<_>>()
        };
    let stale_policy_findings =
        homeboy::core::code_audit::baseline::stale_policy_baseline_fingerprints(
            &baseline,
            &current_policy_fingerprints,
            policy,
            scope,
            baseline_mode,
        )
        .into_iter()
        .filter(|fingerprint| !is_retired_homeboy_domain_policy_fingerprint(fingerprint))
        .collect::<Vec<_>>();

    if baseline_mode == homeboy::core::code_audit::baseline::PolicyBaselineMode::Full {
        assert!(
            !current_policy_findings.is_empty(),
            "core-agnostic source policy should stay configured until #2240/#3195 debt is cleaned up"
        );
    }
    assert!(
        new_policy_findings.is_empty(),
        "core-owned source contains non-baselined ecosystem-specific behavior. Core changes should ship generic, universally useful capabilities rather than framework-specific defaults. {CORE_AGNOSTIC_REPAIR_DIRECTIVE}\nNew audit findings:\n{}",
        new_policy_findings.join("\n")
    );
    assert!(
        stale_policy_findings.is_empty(),
        "core-owned source agnostic audit baseline contains stale entries. Ratchet baselines.audit after cleanup:\n{}",
        stale_policy_findings.join("\n")
    );
}

#[test]
fn core_agnostic_failure_message_directs_extension_boundary_repair() {
    assert!(CORE_AGNOSTIC_REPAIR_DIRECTIVE.contains("not a baseline chore"));
    assert!(CORE_AGNOSTIC_REPAIR_DIRECTIVE.contains("owning extension"));
    assert!(CORE_AGNOSTIC_REPAIR_DIRECTIVE.contains("generic core contract"));
}

fn release_ci_tracks_audit_without_blocking() -> bool {
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        return false;
    }

    let Ok(commands) = std::env::var("RELEASE_BLOCKING_COMMANDS") else {
        return false;
    };

    !commands
        .split(',')
        .map(str::trim)
        .any(|command| command.eq_ignore_ascii_case("audit"))
}

fn is_changed_scope_run() -> bool {
    std::env::var("SCOPE_MODE")
        .map(|value| value == "changed")
        .unwrap_or(false)
}

fn source_policy_finding_in_scope(root: &Path, file: &str) -> bool {
    if !is_changed_scope_run() {
        return true;
    }

    let Ok(changed_since) = std::env::var("HOMEBOY_CHANGED_SINCE") else {
        return true;
    };

    changed_source_files(root, &changed_since)
        .map(|files| {
            files
                .iter()
                .any(|changed| file == changed || file.contains(changed))
        })
        .unwrap_or(true)
}

fn changed_source_files(root: &Path, changed_since: &str) -> Option<BTreeSet<String>> {
    let output = Command::new("git")
        .arg("diff")
        .arg("--name-only")
        .arg(changed_since)
        .arg("--")
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn is_retired_homeboy_domain_policy_fingerprint(fingerprint: &str) -> bool {
    if fingerprint.contains("src/core/extension/runtime/bench-helper.php")
        && fingerprint.contains("configured ecosystem term `php`")
    {
        return true;
    }

    [
        "`Homeboy`",
        "`homeboy.json`",
        "`.homeboy`",
        "`HOMEBOY_`",
        "`homeboy/lab-offload/v1`",
        "`Lab`",
        "`offload`",
        "`homeboy-run`",
    ]
    .iter()
    .any(|term| fingerprint.contains(term))
}

#[test]
fn core_owned_test_content_stays_language_and_framework_agnostic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeMap::<(String, String), Vec<usize>>::new();

    for source_root in CORE_OWNED_SOURCE_ROOTS {
        let path = root.join(source_root);
        if path.is_dir() {
            scan_source_dir_test_content(root, &path, &mut found);
        } else {
            scan_source_file_test_content(root, &path, &mut found);
        }
    }

    for test_root in CORE_OWNED_TEST_CONTENT_ROOTS {
        let path = root.join(test_root);
        if path.exists() {
            scan_test_content_path(root, &path, &mut found);
        }
    }

    assert_test_content_baseline(&found);

    let term_distribution = homeboy::core::top_n::top_n_by(
        found.keys().map(|(_, term)| term.as_str()),
        |term| *term,
        3,
    );
    assert!(
        !term_distribution.is_empty(),
        "test-content baseline should stay explicit until the #3034 cleanup removes existing core-owned fixture leaks"
    );
}

#[test]
fn core_content_stays_free_of_concrete_extension_ids() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeMap::<(String, String), Vec<usize>>::new();

    for source_root in CORE_EXTENSION_BOUNDARY_ROOTS {
        let path = root.join(source_root);
        if path.exists() {
            scan_concrete_extension_ids(root, &path, &mut found);
        }
    }

    let unexpected = found
        .iter()
        .map(|((path, term), lines)| format!("{path}: `{term}` on lines {lines:?}"))
        .collect::<Vec<_>>();

    assert!(
        unexpected.is_empty(),
        "Homeboy core and core-owned tests must not reference concrete extension IDs. Use neutral fixture IDs in core tests and move real extension dependencies into extension-owned tests/config. Concrete extension references found:\n{}",
        unexpected.join("\n")
    );
}

fn scan_source_dir_test_content(
    root: &Path,
    dir: &Path,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    for entry in fs::read_dir(dir).expect("source dir should be readable") {
        let entry = entry.expect("source entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            scan_source_dir_test_content(root, &path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            scan_source_file_test_content(root, &path, found);
        }
    }
}

fn scan_source_file_test_content(
    root: &Path,
    path: &Path,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    if is_extension_owned_path(root, path) {
        return;
    }

    let content = fs::read_to_string(path).expect("source file should be readable");
    let relative = relative_path(root, path);
    let is_helper = is_test_helper(path);
    let mut in_test_content = is_helper;

    for (index, line) in content.lines().enumerate() {
        if line.trim() == "#[cfg(test)]" {
            in_test_content = true;
            continue;
        }
        if !in_test_content {
            continue;
        }

        scan_test_content_line(&relative, index + 1, line, true, found);
    }
}

fn scan_test_content_path(
    root: &Path,
    path: &Path,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    if is_extension_owned_path(root, path) {
        return;
    }

    if path.is_dir() {
        for entry in fs::read_dir(path).expect("test content dir should be readable") {
            let entry = entry.expect("test content entry should be readable");
            scan_test_content_path(root, &entry.path(), found);
        }
        return;
    }

    if !is_scannable_test_content_file(path) {
        return;
    }

    let content = fs::read_to_string(path).expect("test content file should be readable");
    scan_test_content_lines(
        relative_path(root, path),
        content.lines(),
        path.extension().is_some_and(|ext| ext == "rs"),
        found,
    );
}

fn scan_test_content_lines<'a>(
    relative: impl Into<String>,
    lines: impl IntoIterator<Item = &'a str>,
    rust_string_literals_only: bool,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    let relative = relative.into();
    for (index, line) in lines.into_iter().enumerate() {
        scan_test_content_line(&relative, index + 1, line, rust_string_literals_only, found);
    }
}

fn scan_test_content_line(
    relative: &str,
    line_number: usize,
    line: &str,
    rust_string_literals_only: bool,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    let segments = if rust_string_literals_only {
        rust_string_literal_segments(line)
    } else {
        vec![line.to_string()]
    };

    for term in TERMS {
        if segments
            .iter()
            .any(|segment| term.matches_test_content(segment))
        {
            found
                .entry((relative.to_string(), term.name.to_string()))
                .or_default()
                .push(line_number);
        }
    }
}

fn scan_concrete_extension_ids(
    root: &Path,
    path: &Path,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("boundary path should be readable") {
            let entry = entry.expect("boundary entry should be readable");
            scan_concrete_extension_ids(root, &entry.path(), found);
        }
        return;
    }

    if !is_scannable_test_content_file(path) {
        return;
    }

    let content = fs::read_to_string(path).expect("boundary file should be readable");
    let relative = relative_path(root, path);
    let rust_string_literals_only = path.extension().is_some_and(|ext| ext == "rs");

    for (index, line) in content.lines().enumerate() {
        let segments = if rust_string_literals_only {
            rust_string_literal_segments(line)
        } else {
            vec![line.to_string()]
        };
        for term in CONCRETE_EXTENSION_IDS {
            if segments.iter().any(|segment| term.matches(segment)) {
                found
                    .entry((relative.clone(), term.name.to_string()))
                    .or_default()
                    .push(index + 1);
            }
        }
    }
}

fn rust_string_literal_segments(line: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut remaining = line;

    while let Some(start) = remaining.find('"') {
        remaining = &remaining[start + 1..];
        let Some(end) = remaining.find('"') else {
            break;
        };
        segments.push(remaining[..end].to_string());
        remaining = &remaining[end + 1..];
    }

    segments
}

fn assert_test_content_baseline(found: &BTreeMap<(String, String), Vec<usize>>) {
    let baseline = TEST_CONTENT_BASELINE
        .iter()
        .map(|entry| (entry.path.to_string(), entry.term.to_string()))
        .collect::<BTreeSet<_>>();

    let unexpected = found
        .iter()
        .filter(|(key, _)| !baseline.contains(*key))
        .map(|((path, term), lines)| format!("{path}: {term} on lines {lines:?}"))
        .collect::<Vec<_>>();
    let debt_report = format_test_content_debt_report(found);

    assert!(
        unexpected.is_empty(),
        "core-owned test content contains non-baselined ecosystem fixture language:\n{}\n\nUse generic fixtures/examples in core-owned tests, move ecosystem-specific cases into extension-owned tests, or add a narrow issue-linked TEST_CONTENT_BASELINE entry for unavoidable current debt.\n\n{}",
        unexpected.join("\n"),
        debt_report
    );

    let stale_baseline = TEST_CONTENT_BASELINE
        .iter()
        .filter(|entry| !found.contains_key(&(entry.path.to_string(), entry.term.to_string())))
        .map(|entry| format!("- {}: {}", entry.path, entry.term))
        .collect::<Vec<_>>();
    assert!(
        stale_baseline.is_empty(),
        "core-owned test content ecosystem baseline contains stale entries. Remove stale TEST_CONTENT_BASELINE entries:\n{}\n\n{}",
        stale_baseline.join("\n"),
        debt_report
    );

    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    assert_eq!(
        occurrence_count, TEST_CONTENT_BASELINE_OCCURRENCES,
        "core-owned test content ecosystem baseline occurrence count changed. If this went down, lower TEST_CONTENT_BASELINE_OCCURRENCES and remove stale rows. If it went up, move the fixture language into extension-owned tests.\n\n{}",
        debt_report
    );
}

fn is_scannable_test_content_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext,
                "rs" | "json" | "jsonl" | "toml" | "yaml" | "yml" | "md" | "txt"
            )
        })
}

fn is_extension_owned_path(root: &Path, path: &Path) -> bool {
    relative_path(root, path)
        .split('/')
        .any(|component| component == "extensions")
}

fn is_test_helper(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    file_name == "tests.rs"
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_tests.rs")
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn format_test_content_debt_report(found: &BTreeMap<(String, String), Vec<usize>>) -> String {
    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    let paths = found
        .keys()
        .map(|(path, _)| path.as_str())
        .collect::<BTreeSet<_>>();

    format!(
        "Core-owned test content ecosystem debt (#3034): {occurrence_count} occurrences across {} path/term pairs and {} files. Current rows:\n{}",
        found.len(),
        paths.len(),
        first_counted_rows(
            found
                .iter()
                .map(|((path, term), lines)| (
                    lines.len(),
                    format!("- {path}: {term} on lines {lines:?}")
                ))
                .collect::<Vec<_>>()
        )
    )
}

fn first_counted_rows(mut rows: Vec<(usize, String)>) -> String {
    sort_counted_rows_desc(&mut rows);

    rows.into_iter()
        .take(10)
        .map(|(_, row)| row)
        .collect::<Vec<_>>()
        .join("\n")
}

fn sort_counted_rows_desc(rows: &mut [(usize, String)]) {
    rows.sort_by(|(left_count, left), (right_count, right)| {
        right_count.cmp(left_count).then_with(|| left.cmp(right))
    });
}

impl Term {
    fn matches(self, line: &str) -> bool {
        match self.kind {
            MatchKind::Literal => line.contains(self.name),
            MatchKind::Token => contains_token(line, self.name),
        }
    }

    fn matches_test_content(self, line: &str) -> bool {
        matches!(self.kind, MatchKind::Token)
            && !is_source_only_homeboy_domain_term(self.name)
            && contains_test_content_variant(line, self.name)
    }
}

fn is_source_only_homeboy_domain_term(term: &str) -> bool {
    matches!(term, "Homeboy" | "Lab" | "offload")
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(offset) = haystack[search_from..].find(needle) {
        let start = search_from + offset;
        let end = start + needle.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();

        if !is_word_char(before) && !is_word_char(after) {
            return true;
        }

        search_from = end;
    }

    false
}

fn is_word_char(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn contains_test_content_variant(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    let mut search_from = 0;

    while let Some(offset) = haystack[search_from..].find(&needle) {
        let start = search_from + offset;
        let end = start + needle.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();

        if is_test_content_separator(before) || is_test_content_separator(after) {
            return true;
        }

        search_from = end;
    }

    false
}

fn is_test_content_separator(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| ch == '_' || ch == '-')
}
