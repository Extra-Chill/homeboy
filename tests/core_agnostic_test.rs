use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

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
];

// Baseline mode for issue #2241 while the cleanup wave in #2240 lands.
// Each entry is a known production-code leak in core-owned source. Fixtures and
// examples are not listed here: the scanner skips Rust test modules and source
// test helpers instead of allowing broad paths like `tests/**`.
const BASELINE: &[ViolationKey] = &[
    ViolationKey {
        path: "src/commands/component.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/commands/doctor/resources.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/commands/doctor/resources.rs",
        term: "npm",
    },
    ViolationKey {
        path: "src/commands/doctor/resources.rs",
        term: "phpcs",
    },
    ViolationKey {
        path: "src/commands/doctor/resources.rs",
        term: "phpstan",
    },
    ViolationKey {
        path: "src/commands/doctor/resources.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/commands/extension.rs",
        term: "phpcs",
    },
    ViolationKey {
        path: "src/commands/extension.rs",
        term: "phpstan",
    },
    ViolationKey {
        path: "src/commands/lint.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/commands/test.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/code_audit/codebase_map.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/conventions.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/dead_code.rs",
        term: "WP_CLI",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        term: "composer.json",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        term: "composer.json",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/docs_audit/claims.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/docs_audit/claims.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/code_audit/docs_audit/verify.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/field_patterns.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/repeated_literal_shape.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/requested_detectors.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/requested_detectors.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/code_audit/requirements.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/code_audit/requirements.rs",
        term: "composer.json",
    },
    ViolationKey {
        path: "src/core/code_audit/requirements.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/shared_scaffolding.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/structural.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/upstream_workaround.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/code_audit/walker.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/code_audit/detectors/wrapper_inference.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/component/mod.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/component/mod.rs",
        term: "npm",
    },
    ViolationKey {
        path: "src/core/defaults.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/defaults/builtins.rs",
        term: "Cargo.toml",
    },
    ViolationKey {
        path: "src/core/defaults/builtins.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/defaults/builtins.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/defaults/builtins.rs",
        term: "composer.json",
    },
    ViolationKey {
        path: "src/core/defaults/builtins.rs",
        term: "package.json",
    },
    ViolationKey {
        path: "src/core/defaults/builtins.rs",
        term: "style.css",
    },
    ViolationKey {
        path: "src/core/deploy/permissions.rs",
        term: "wp-content",
    },
    ViolationKey {
        path: "src/core/deps.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/deps.rs",
        term: "composer.json",
    },
    ViolationKey {
        path: "src/core/deps.rs",
        term: "npm",
    },
    ViolationKey {
        path: "src/core/deps.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/engine/codebase_scan.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/engine/edit_op_apply.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/engine/executor.rs",
        term: "composer",
    },
    ViolationKey {
        path: "src/core/engine/executor.rs",
        term: "npm",
    },
    ViolationKey {
        path: "src/core/engine/symbol_graph.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/engine/symbol_graph.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/extension/grammar.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/grammar.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/extension/grammar.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/extension/lifecycle.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/extension/manifest.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/extension/manifest.rs",
        term: "npx",
    },
    ViolationKey {
        path: "src/core/extension/manifest.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/manifest.rs",
        term: "phpcbf",
    },
    ViolationKey {
        path: "src/core/extension/manifest.rs",
        term: "phpcs",
    },
    ViolationKey {
        path: "src/core/extension/manifest.rs",
        term: "phpstan",
    },
    ViolationKey {
        path: "src/core/extension/runtime_helper.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/runtime_helper/assets.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/test/drift.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/test/drift.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/extension/test/mod.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/extension/test/report.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/extension/test/run.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/git/commits.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/git/primitives.rs",
        term: "wordpress",
    },
    ViolationKey {
        path: "src/core/project/mod.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/refactor/decompose.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/refactor/move_items.rs",
        term: "rust",
    },
    ViolationKey {
        path: "src/core/refactor/plan/generate/duplicate_fixes.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/refactor/plan/generate/signatures.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/refactor/plan/sources.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/refactor/transform.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/release/planning_quality.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/release/version/default_pattern_for_file.rs",
        term: "php",
    },
    ViolationKey {
        path: "src/core/rig/toolchain.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/self_status.rs",
        term: "Cargo.toml",
    },
    ViolationKey {
        path: "src/core/self_status.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/upgrade/execution.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/upgrade/helpers.rs",
        term: "cargo",
    },
    ViolationKey {
        path: "src/core/upgrade/mod.rs",
        term: "cargo",
    },
];

const BASELINE_OCCURRENCES: usize = 157;

#[test]
fn core_owned_source_stays_language_and_framework_agnostic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeMap::<(String, String), Vec<usize>>::new();

    for source_root in CORE_OWNED_SOURCE_ROOTS {
        let path = root.join(source_root);
        if path.is_dir() {
            scan_dir(root, &path, &mut found);
        } else {
            scan_file(root, &path, &mut found);
        }
    }

    let baseline = BASELINE
        .iter()
        .map(|entry| (entry.path.to_string(), entry.term.to_string()))
        .collect::<BTreeSet<_>>();

    let unexpected = found
        .iter()
        .filter(|(key, _)| !baseline.contains(*key))
        .map(|((path, term), lines)| format!("{path}: {term} on lines {lines:?}"))
        .collect::<Vec<_>>();
    let debt_report = format_baseline_debt_report(&found);

    assert!(
        unexpected.is_empty(),
        "core-owned source contains non-baselined ecosystem behavior:\n{}\n\nAdd extension-owned behavior instead, or update the narrow baseline only for known issue #2240 cleanup violations.\n\n{}",
        unexpected.join("\n"),
        debt_report
    );

    let stale_baseline = stale_baseline_rows(&found);
    assert!(
        stale_baseline.is_empty(),
        "core-owned source ecosystem baseline contains stale entries. Remove stale BASELINE entries so the #2240 guard only allows current debt:\n{}\n\n{}",
        stale_baseline.join("\n"),
        debt_report
    );

    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    assert_eq!(
        occurrence_count, BASELINE_OCCURRENCES,
        "core-owned source ecosystem baseline occurrence count changed. If this went down, lower BASELINE_OCCURRENCES and remove stale BASELINE entries. If it went up, move behavior into an extension-owned layer.\n\n{}",
        debt_report
    );

    let term_distribution = homeboy::core::top_n::top_n_by(
        found.keys().map(|(_, term)| term.as_str()),
        |term| *term,
        3,
    );
    assert!(
        !term_distribution.is_empty(),
        "baseline should stay explicit until the #2240 cleanup removes existing core leaks"
    );
}

fn scan_dir(root: &Path, dir: &Path, found: &mut BTreeMap<(String, String), Vec<usize>>) {
    for entry in fs::read_dir(dir).expect("source dir should be readable") {
        let entry = entry.expect("source entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            scan_dir(root, &path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            scan_file(root, &path, found);
        }
    }
}

fn scan_file(root: &Path, path: &Path, found: &mut BTreeMap<(String, String), Vec<usize>>) {
    if is_test_helper(path) {
        return;
    }

    let content = fs::read_to_string(path).expect("source file should be readable");
    let relative = relative_path(root, path);
    let mut skip_rest_as_test_module = false;

    for (index, line) in content.lines().enumerate() {
        if line.trim() == "#[cfg(test)]" {
            skip_rest_as_test_module = true;
            continue;
        }
        if skip_rest_as_test_module {
            continue;
        }

        for term in TERMS {
            if term.matches(line) {
                found
                    .entry((relative.clone(), term.name.to_string()))
                    .or_default()
                    .push(index + 1);
            }
        }
    }
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

fn format_baseline_debt_report(found: &BTreeMap<(String, String), Vec<usize>>) -> String {
    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    let paths = found
        .keys()
        .map(|(path, _)| path.as_str())
        .collect::<BTreeSet<_>>();
    let mut by_term = BTreeMap::<&str, (usize, BTreeSet<&str>)>::new();
    let mut by_path = BTreeMap::<&str, (usize, BTreeSet<&str>)>::new();

    for ((path, term), lines) in found {
        let count = lines.len();
        let term_entry = by_term.entry(term.as_str()).or_default();
        term_entry.0 += count;
        term_entry.1.insert(path.as_str());

        let path_entry = by_path.entry(path.as_str()).or_default();
        path_entry.0 += count;
        path_entry.1.insert(term.as_str());
    }

    let mut term_rows = by_term
        .iter()
        .map(|(term, (count, paths))| {
            (
                *count,
                format!("- {term}: {count} occurrences across {} files", paths.len()),
            )
        })
        .collect::<Vec<_>>();
    sort_counted_rows_desc(&mut term_rows);

    let mut path_rows = by_path
        .iter()
        .map(|(path, (count, terms))| {
            (
                *count,
                format!(
                    "- {path}: {count} occurrences across {} terms ({})",
                    terms.len(),
                    terms.iter().copied().collect::<Vec<_>>().join(", ")
                ),
            )
        })
        .collect::<Vec<_>>();
    sort_counted_rows_desc(&mut path_rows);

    format!(
        "Core-agnostic baseline debt (#2240): {occurrence_count} occurrences across {} path/term pairs and {} files.\nTop terms:\n{}\nTop files:\n{}\nStale baseline entries to prune after cleanup:\n{}",
        found.len(),
        paths.len(),
        first_counted_rows(term_rows),
        first_counted_rows(path_rows),
        stale_baseline_report(found)
    )
}

fn stale_baseline_report(found: &BTreeMap<(String, String), Vec<usize>>) -> String {
    let stale_rows = stale_baseline_rows(found);

    if stale_rows.is_empty() {
        return "- none".to_string();
    }

    first_rows(stale_rows)
}

fn stale_baseline_rows(found: &BTreeMap<(String, String), Vec<usize>>) -> Vec<String> {
    BASELINE
        .iter()
        .filter(|entry| !found.contains_key(&(entry.path.to_string(), entry.term.to_string())))
        .map(|entry| format!("- {}: {}", entry.path, entry.term))
        .collect::<Vec<_>>()
}

fn first_rows(rows: Vec<String>) -> String {
    rows.into_iter().take(10).collect::<Vec<_>>().join("\n")
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
