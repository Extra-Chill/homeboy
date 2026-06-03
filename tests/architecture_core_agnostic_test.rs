use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
struct DetectorAgnosticTerm {
    name: &'static str,
    kind: DetectorAgnosticMatchKind,
}

#[derive(Clone, Copy)]
enum DetectorAgnosticMatchKind {
    Literal,
    Token,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct DetectorAgnosticBaseline {
    path: &'static str,
    token: &'static str,
}

const DETECTOR_AGNOSTIC_TERMS: &[DetectorAgnosticTerm] = &[
    DetectorAgnosticTerm {
        name: "Rust",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "rust",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "PHP",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "php",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "Node",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "nodejs",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "Cargo",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "cargo",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "Composer",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "composer",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "npm",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "WP",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "WordPress",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "wordpress",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "Homeboy",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "homeboy",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "Lab",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "offload",
        kind: DetectorAgnosticMatchKind::Token,
    },
    DetectorAgnosticTerm {
        name: "homeboy-run",
        kind: DetectorAgnosticMatchKind::Literal,
    },
    DetectorAgnosticTerm {
        name: "Cargo.toml",
        kind: DetectorAgnosticMatchKind::Literal,
    },
    DetectorAgnosticTerm {
        name: "Cargo.lock",
        kind: DetectorAgnosticMatchKind::Literal,
    },
    DetectorAgnosticTerm {
        name: "composer.json",
        kind: DetectorAgnosticMatchKind::Literal,
    },
    DetectorAgnosticTerm {
        name: "package.json",
        kind: DetectorAgnosticMatchKind::Literal,
    },
    DetectorAgnosticTerm {
        name: "wp-content",
        kind: DetectorAgnosticMatchKind::Literal,
    },
    DetectorAgnosticTerm {
        name: "runner-artifact://",
        kind: DetectorAgnosticMatchKind::Literal,
    },
];

// Known detector implementation debt is tracked in #2838, #2839, #2841,
// and #2842. Keep this baseline narrow: it exists only to fail regressions
// while those child issues move hardcoded behavior into config/extension layers.
const DETECTOR_AGNOSTIC_BASELINE: &[DetectorAgnosticBaseline] = &[
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/aggregate_construction.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/aggregate_construction.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/command_status_contracts.rs",
        token: "Homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/command_status_contracts.rs",
        token: "homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        token: "composer",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        token: "composer.json",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/dead_guard.rs",
        token: "php",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        token: "composer",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        token: "composer.json",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/deprecation_age.rs",
        token: "php",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/facade_passthrough.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/field_patterns.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/field_patterns.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/field_patterns.rs",
        token: "php",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/global_env_guard.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/layer_ownership.rs",
        token: "homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/repeated_literal_shape.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/repeated_literal_shape.rs",
        token: "php",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/rust_test_wiring.rs",
        token: "Cargo",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/rust_test_wiring.rs",
        token: "Homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/rust_test_wiring.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/shared_scaffolding.rs",
        token: "php",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_coverage.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_coverage.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_coverage.rs",
        token: "homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_coverage.rs",
        token: "php",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_topology.rs",
        token: "homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_vacuity.rs",
        token: "Cargo",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_vacuity.rs",
        token: "Cargo.toml",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/test_vacuity.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/unbounded_output_capture.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/upstream_workaround.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/upstream_workaround.rs",
        token: "WP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/upstream_workaround.rs",
        token: "wordpress",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/wrapper_inference.rs",
        token: "PHP",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/wrapper_inference.rs",
        token: "Rust",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/wrapper_inference.rs",
        token: "homeboy",
    },
    DetectorAgnosticBaseline {
        path: "src/core/code_audit/detectors/wrapper_inference.rs",
        token: "php",
    },
];
const DETECTOR_AGNOSTIC_BASELINE_OCCURRENCES: usize = 77;

fn source_file(relative_path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    std::fs::read_to_string(path).expect("read source file")
}

#[test]
fn deploy_archive_core_stays_free_of_wordpress_header_semantics() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    let forbidden = [
        "Plugin Name",
        "Theme Name",
        "Text Domain",
        "Requires at least",
        "Requires PHP",
        "Tested up to",
        "Stable tag",
        "style.css",
    ];
    let scanned_roots = ["src/core/deploy", "tests/commands/deploy_test.rs"];

    for relative_path in scanned_roots {
        validate_files_for_forbidden_literals(
            root,
            &root.join(relative_path),
            &forbidden,
            &mut violations,
        );
    }

    assert!(
        violations.is_empty(),
        "core deploy/archive implementation and tests must not bake in WordPress plugin/theme header semantics. Keep domain-specific archive verification in extension-owned config. Violations:\n{}",
        violations.join("\n")
    );
}

fn validate_files_for_forbidden_literals(
    root: &std::path::Path,
    path: &std::path::Path,
    forbidden: &[&str],
    violations: &mut Vec<String>,
) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("read source directory") {
            let entry = entry.expect("read source entry");
            validate_files_for_forbidden_literals(root, &entry.path(), forbidden, violations);
        }
        return;
    }

    if path.extension().is_none_or(|extension| extension != "rs") {
        return;
    }

    let relative = relative_source_path(root, path);
    let content = std::fs::read_to_string(path).expect("read source file");
    for (index, line) in content.lines().enumerate() {
        for term in forbidden {
            if line.contains(term) {
                violations.push(format!("{relative}:{} contains `{term}`", index + 1));
            }
        }
    }
}

#[test]
fn core_source_does_not_depend_on_command_layer() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let core_root = root.join("src/core");
    let forbidden = [
        "use crate::commands",
        "crate::commands::",
        "use homeboy::commands",
        "homeboy::commands::",
        "use crate::cli_surface",
        "crate::cli_surface::",
        "use homeboy::cli_surface",
        "homeboy::cli_surface::",
    ];
    let mut violations = Vec::new();

    scan_core_source_for_command_layer(root, &core_root, &forbidden, &mut violations);

    assert!(
        violations.is_empty(),
        "core source must not depend on the command/CLI layer:\n{}\n\nMove command parsing/execution behind an injected adapter owned by src/commands.",
        violations.join("\n")
    );
}

fn scan_core_source_for_command_layer(
    root: &std::path::Path,
    path: &std::path::Path,
    forbidden: &[&str],
    violations: &mut Vec<String>,
) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("read core source directory") {
            let entry = entry.expect("read core source entry");
            scan_core_source_for_command_layer(root, &entry.path(), forbidden, violations);
        }
        return;
    }

    if path.extension().is_none_or(|extension| extension != "rs") {
        return;
    }

    let content = std::fs::read_to_string(path).expect("read core source file");
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let mut skip_rest_as_test_module = false;

    for (index, line) in content.lines().enumerate() {
        if line.trim() == "#[cfg(test)]" {
            skip_rest_as_test_module = true;
            continue;
        }
        if skip_rest_as_test_module {
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }

        for term in forbidden {
            if line.contains(term) {
                violations.push(format!("{relative}:{} contains `{term}`", index + 1));
            }
        }
    }
}

#[test]
fn library_root_does_not_flatten_core_surface() {
    let source = source_file("src/lib.rs");

    assert!(
        !source.contains("pub use core::*"),
        "src/lib.rs must keep core APIs behind homeboy::core instead of flattening the crate root"
    );
}

#[test]
fn server_root_does_not_wildcard_reexport_private_modules() {
    let source = source_file("src/core/server/mod.rs");

    assert!(
        !source.contains("pub use client::*")
            && !source.contains("pub use connection::*")
            && !source.contains("pub use keys::*")
            && !source.contains("pub use session::*"),
        "src/core/server/mod.rs must explicitly name the server APIs it re-exports"
    );
}

#[test]
fn release_version_root_does_not_wildcard_reexport_private_modules() {
    let source = source_file("src/core/release/version.rs");

    assert!(
        !source.contains("pub use default_pattern_for_file::*")
            && !source.contains("pub use types::*")
            && !source.contains("pub use version::*"),
        "src/core/release/version.rs must explicitly name the version APIs it re-exports"
    );
}

#[test]
fn release_changelog_roots_do_not_wildcard_reexport_private_modules() {
    let roots = [
        "src/core/release/changelog/mod.rs",
        "src/core/release/changelog/sections.rs",
    ];

    for root in roots {
        let source = source_file(root);
        assert!(
            !source.contains("pub use bulk::*")
                && !source.contains("pub use io::*")
                && !source.contains("pub use sections::*")
                && !source.contains("pub use settings::*")
                && !source.contains("pub use normalize_heading_label::*")
                && !source.contains("pub use unreleased::*"),
            "{root} must explicitly name the changelog APIs it re-exports"
        );
    }
}

#[test]
fn git_root_does_not_wildcard_reexport_private_modules() {
    let source = source_file("src/core/git/mod.rs");

    assert!(
        !source.contains("pub use changes::*")
            && !source.contains("pub use commits::*")
            && !source.contains("pub use github::*")
            && !source.contains("pub use operations::*")
            && !source.contains("pub use pr_policy::*")
            && !source.contains("pub use primitives::*"),
        "src/core/git/mod.rs must explicitly name the git APIs it re-exports"
    );
}

#[test]
fn validate_and_format_writes_do_not_select_ecosystem_commands() {
    let files = [
        "src/core/engine/validate_write.rs",
        "src/core/engine/format_write.rs",
    ];
    let forbidden = [
        "Cargo.toml",
        "cargo check",
        "cargo fmt",
        "tsconfig.json",
        "npx tsc",
        "prettier",
        "go vet",
        "gofmt",
        "phpcbf",
        "rustfmt",
    ];

    for file in files {
        let source = source_file(file);
        for term in forbidden {
            assert!(
                !source.contains(term),
                "{file} must not hardcode ecosystem command or marker `{term}`"
            );
        }
    }
}

#[test]
fn detector_implementations_stay_domain_agnostic() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let detectors_root = root.join("src/core/code_audit/detectors");
    let mut found = BTreeMap::<(String, String), Vec<usize>>::new();

    scan_detector_source_for_domain_tokens(root, &detectors_root, &mut found);

    let baseline = DETECTOR_AGNOSTIC_BASELINE
        .iter()
        .map(|entry| (entry.path.to_string(), entry.token.to_string()))
        .collect::<BTreeSet<_>>();

    let unexpected = found
        .iter()
        .filter(|(key, _)| !baseline.contains(*key))
        .map(|((path, token), lines)| format!("{path}: `{token}` on lines {lines:?}"))
        .collect::<Vec<_>>();

    assert!(
        unexpected.is_empty(),
        "core audit detector implementations must stay codebase/language/domain agnostic. Move hardcoded behavior into extension-owned configuration or fixture-only tests. Non-baselined detector tokens:\n{}\n\n{}",
        unexpected.join("\n"),
        detector_agnostic_debt_report(&found)
    );

    let stale_baseline = DETECTOR_AGNOSTIC_BASELINE
        .iter()
        .filter(|entry| !found.contains_key(&(entry.path.to_string(), entry.token.to_string())))
        .map(|entry| format!("{}: {}", entry.path, entry.token))
        .collect::<Vec<_>>();

    assert!(
        stale_baseline.is_empty(),
        "detector agnostic baseline contains stale entries. Remove fixed child-issue debt from DETECTOR_AGNOSTIC_BASELINE:\n{}",
        stale_baseline.join("\n")
    );

    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    assert_eq!(
        occurrence_count,
        DETECTOR_AGNOSTIC_BASELINE_OCCURRENCES,
        "detector agnostic baseline occurrence count changed. If this went down, lower DETECTOR_AGNOSTIC_BASELINE_OCCURRENCES and remove stale baseline rows. If it went up, move hardcoded behavior out of detector implementation files.\n\n{}",
        detector_agnostic_debt_report(&found)
    );

    let top_tokens = homeboy::core::top_n::top_n_by(
        found.keys().map(|(_, token)| token.as_str()),
        |token| *token,
        3,
    );
    assert!(
        !top_tokens.is_empty(),
        "baseline should stay explicit until #2836 child issues remove existing detector debt"
    );
}

fn scan_detector_source_for_domain_tokens(
    root: &std::path::Path,
    path: &std::path::Path,
    found: &mut BTreeMap<(String, String), Vec<usize>>,
) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("read detector source directory") {
            let entry = entry.expect("read detector source entry");
            scan_detector_source_for_domain_tokens(root, &entry.path(), found);
        }
        return;
    }

    if path.extension().is_none_or(|extension| extension != "rs") {
        return;
    }

    let content = std::fs::read_to_string(path).expect("read detector source file");
    let relative = relative_source_path(root, path);
    let mut skip_rest_as_test_module = false;

    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "#[cfg(test)]" {
            skip_rest_as_test_module = true;
            continue;
        }
        if skip_rest_as_test_module || is_generic_config_handling_line(trimmed) {
            continue;
        }

        for term in DETECTOR_AGNOSTIC_TERMS {
            if detector_line_contains_term(line, *term) {
                found
                    .entry((relative.clone(), term.name.to_string()))
                    .or_default()
                    .push(index + 1);
            }
        }
    }
}

fn is_generic_config_handling_line(trimmed: &str) -> bool {
    trimmed.contains("config.")
        || trimmed.contains("&config")
        || trimmed.contains("Config")
        || trimmed.contains("configured")
}

fn relative_source_path(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn detector_agnostic_debt_report(found: &BTreeMap<(String, String), Vec<usize>>) -> String {
    let occurrence_count = found.values().map(Vec::len).sum::<usize>();
    let mut rows = found
        .iter()
        .map(|((path, token), lines)| {
            (
                lines.len(),
                format!("- {path}: `{token}` on lines {lines:?}"),
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|(left_count, left), (right_count, right)| {
        right_count.cmp(left_count).then_with(|| left.cmp(right))
    });

    format!(
        "Detector agnostic debt (#2836): {occurrence_count} occurrences across {} path/token pairs. Current rows:\n{}",
        found.len(),
        rows.into_iter()
            .take(20)
            .map(|(_, row)| row)
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn detector_line_contains_term(line: &str, term: DetectorAgnosticTerm) -> bool {
    if matches!(term.kind, DetectorAgnosticMatchKind::Literal) {
        return line.contains(term.name);
    }

    contains_detector_token(line, term.name)
}

fn contains_detector_token(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(offset) = haystack[search_from..].find(needle) {
        let start = search_from + offset;
        let end = start + needle.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();

        if !is_detector_token_char(before) && !is_detector_token_char(after) {
            return true;
        }

        search_from = end;
    }

    false
}

fn is_detector_token_char(ch: Option<char>) -> bool {
    ch.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}
