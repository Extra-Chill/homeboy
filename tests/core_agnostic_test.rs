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

// Baseline mode for issue #2241 while the cleanup wave in #2240 lands.
// Issue #3195 extends this guard to Homeboy-domain product assumptions in
// core-owned source. Generic concepts like command, artifact, capability,
// preflight, and runner can remain in core; product/domain values should come
// from configuration, extension manifests, or typed extension contracts.
// Each entry is a known production-code leak in core-owned source. Fixtures and
// examples are not listed here: the scanner skips Rust test modules and source
// test helpers instead of allowing broad paths like `tests/**`.
const BASELINE: &str = include_str!("core_agnostic_data/source_baseline.txt");

const BASELINE_OCCURRENCES: usize = 660;

// Known core-owned test/fixture literal debt tracked by #3034. Keep this list
// explicit so stale rows and occurrence-count changes force cleanup or review.
const TEST_CONTENT_BASELINE: &str = include_str!("core_agnostic_data/test_content_baseline.txt");

const TEST_CONTENT_BASELINE_OCCURRENCES: usize = 104;

#[test]
fn core_owned_source_stays_language_and_framework_agnostic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeMap::<(String, String), Vec<usize>>::new();

    for source_root in CORE_OWNED_SOURCE_ROOTS {
        let path = root.join(source_root);
        if path.is_dir() {
            helpers::scan_dir(root, &path, &mut found);
        } else {
            helpers::scan_file(root, &path, &mut found);
        }
    }

    let baseline = helpers::baseline_entries(BASELINE);

    let unexpected = found
        .iter()
        .filter(|(key, _)| !baseline.contains(*key))
        .map(|((path, term), lines)| format!("{path}: {term} on lines {lines:?}"))
        .collect::<Vec<_>>();
    let debt_report = helpers::format_baseline_debt_report(&found);

    assert!(
        unexpected.is_empty(),
        "core-owned source contains non-baselined ecosystem or Homeboy-domain behavior:\n{}\n\nCore concepts are allowed when generic (command, artifact, capability, preflight, runner), but product/domain values must come from config, extension manifests, or typed extension contracts. Add extension-owned behavior instead, or update the narrow baseline only for known issue-linked cleanup violations (#2240 or #3195).\n\n{}",
        unexpected.join("\n"),
        debt_report
    );

    let stale_baseline = helpers::stale_baseline_rows(&found);
    assert!(
        stale_baseline.is_empty(),
        "core-owned source agnostic baseline contains stale entries. Remove stale BASELINE entries so the #2240/#3195 guard only allows current debt:\n{}\n\n{}",
        stale_baseline.join("\n"),
        debt_report
    );

    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    assert_eq!(
        occurrence_count, BASELINE_OCCURRENCES,
        "core-owned source agnostic baseline occurrence count changed. If this went down, lower BASELINE_OCCURRENCES and remove stale BASELINE entries. If it went up, move behavior into config, extension manifests, or typed extension contracts.\n\n{}",
        debt_report
    );

    let term_distribution = homeboy::core::top_n::top_n_by(
        found.keys().map(|(_, term)| term.as_str()),
        |term| *term,
        3,
    );
    assert!(
        !term_distribution.is_empty(),
        "baseline should stay explicit until the #2240/#3195 cleanup removes existing core leaks"
    );
}

#[test]
fn core_owned_test_content_stays_language_and_framework_agnostic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeMap::<(String, String), Vec<usize>>::new();

    for source_root in CORE_OWNED_SOURCE_ROOTS {
        let path = root.join(source_root);
        if path.is_dir() {
            helpers::scan_source_dir_test_content(root, &path, &mut found);
        } else {
            helpers::scan_source_file_test_content(root, &path, &mut found);
        }
    }

    for test_root in CORE_OWNED_TEST_CONTENT_ROOTS {
        let path = root.join(test_root);
        if path.exists() {
            helpers::scan_test_content_path(root, &path, &mut found);
        }
    }

    helpers::assert_test_content_baseline(&found);

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

mod helpers {
    use super::*;

    pub(super) fn baseline_entries(data: &str) -> BTreeSet<(String, String)> {
        data.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let (path, term) = line
                    .split_once('\t')
                    .expect("baseline row should contain a tab separator");
                (path.to_string(), term.to_string())
            })
            .collect()
    }

    pub(super) fn scan_dir(
        root: &Path,
        dir: &Path,
        found: &mut BTreeMap<(String, String), Vec<usize>>,
    ) {
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

    pub(super) fn scan_file(
        root: &Path,
        path: &Path,
        found: &mut BTreeMap<(String, String), Vec<usize>>,
    ) {
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

    pub(super) fn scan_source_dir_test_content(
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

    pub(super) fn scan_source_file_test_content(
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

    pub(super) fn scan_test_content_path(
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

    pub(super) fn assert_test_content_baseline(found: &BTreeMap<(String, String), Vec<usize>>) {
        let baseline = baseline_entries(TEST_CONTENT_BASELINE);

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

        let stale_baseline = baseline_entries(TEST_CONTENT_BASELINE)
            .into_iter()
            .filter(|entry| !found.contains_key(entry))
            .map(|(path, term)| format!("- {path}: {term}"))
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

    pub(super) fn format_baseline_debt_report(
        found: &BTreeMap<(String, String), Vec<usize>>,
    ) -> String {
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
        "Core-agnostic baseline debt (#2240/#3195): {occurrence_count} occurrences across {} path/term pairs and {} files.\nTop terms:\n{}\nTop files:\n{}\nStale baseline entries to prune after cleanup:\n{}",
        found.len(),
        paths.len(),
        first_counted_rows(term_rows),
        first_counted_rows(path_rows),
        stale_baseline_report(found)
    )
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

    fn stale_baseline_report(found: &BTreeMap<(String, String), Vec<usize>>) -> String {
        let stale_rows = stale_baseline_rows(found);

        if stale_rows.is_empty() {
            return "- none".to_string();
        }

        first_rows(stale_rows)
    }

    pub(super) fn stale_baseline_rows(
        found: &BTreeMap<(String, String), Vec<usize>>,
    ) -> Vec<String> {
        baseline_entries(BASELINE)
            .into_iter()
            .filter(|entry| !found.contains_key(entry))
            .map(|(path, term)| format!("- {path}: {term}"))
            .collect()
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
}
