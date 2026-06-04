use std::collections::BTreeSet;

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
    let relative = relative_source_path(root, path);
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
    let findings = homeboy::core::code_audit::source_policy_findings_for_path(
        "homeboy",
        root.to_str().expect("manifest dir is UTF-8"),
    )
    .expect("source policy should run");
    let baseline = homeboy::core::code_audit::baseline::load_baseline(root)
        .expect("homeboy.json should contain an audit baseline");

    let policy = "core_boundary_leak:detector-agnostic-source";
    let current_policy_findings = findings
        .iter()
        .filter(|finding| finding.convention == policy)
        .collect::<Vec<_>>();

    let baseline_fingerprints = baseline
        .known_fingerprints
        .iter()
        .map(|fingerprint| fingerprint.as_str())
        .collect::<BTreeSet<_>>();
    let current_policy_fingerprints = current_policy_findings
        .iter()
        .map(|finding| homeboy::core::code_audit::baseline::finding_baseline_fingerprint(finding))
        .collect::<BTreeSet<_>>();

    let new_policy_findings = current_policy_findings
        .iter()
        .filter_map(|finding| {
            let fingerprint =
                homeboy::core::code_audit::baseline::finding_baseline_fingerprint(finding);
            (!baseline_fingerprints.contains(fingerprint.as_str()))
                .then(|| format!("{}: {}", fingerprint, finding.description))
        })
        .collect::<Vec<_>>();
    let stale_policy_findings = baseline
        .known_fingerprints
        .iter()
        .filter(|fingerprint| {
            fingerprint.starts_with(policy)
                && !current_policy_fingerprints.contains(fingerprint.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        !current_policy_findings.is_empty(),
        "detector-agnostic source policy should stay configured until #2836 child-issue debt is cleaned up"
    );
    assert!(
        new_policy_findings.is_empty(),
        "core audit detector implementations must stay codebase/language/domain agnostic. Move hardcoded behavior into extension-owned configuration or fixture-only tests. New audit findings:\n{}",
        new_policy_findings.join("\n")
    );
    assert!(
        stale_policy_findings.is_empty(),
        "detector-agnostic audit baseline contains stale entries. Ratchet baselines.audit after cleanup:\n{}",
        stale_policy_findings.join("\n")
    );
}

fn relative_source_path(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
