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
fn command_layer_uses_explicit_core_facades_only() {
    // Commands and CLI surface must depend on the explicit facade groups
    // (`core::agent_tasks::*`, `core::runners::*`, `core::artifacts::*`) and
    // must not reach into the underlying implementation modules. The facade
    // modules are an intentional API contract; implementation layout below
    // them must be free to change without breaking the command layer.
    //
    // Allowed imports:
    // - `homeboy::core::agent_tasks` and its nested groups (cook_loop,
    //   finalization, gate, lifecycle, loop_controller, promotion, provider,
    //   scheduler, secrets, service).
    // - `homeboy::core::runners` and its nested groups (registry, connection,
    //   execution, workspace, evidence, capabilities, lab_offload).
    // - `homeboy::core::artifacts`.
    //
    // Blocked imports (implementation files that the facades wrap):
    // - `homeboy::core::agent_task`, `homeboy::core::agent_task_*` (any of the
    //   per-operation implementation files such as `agent_task_lifecycle`,
    //   `agent_task_service`, `agent_task_scheduler`, etc.).
    // - `homeboy::core::runner` and its submodules.
    // - `homeboy::core::artifact_*` (e.g. `artifact_links`, `artifact_manifest`).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let scanned_roots = [
        "src/commands",
        "src/cli_runtime.rs",
        "src/cli_surface.rs",
        "src/command_contract.rs",
        "src/command_contract",
    ];
    let forbidden = [
        // agent task implementation modules
        "homeboy::core::agent_task::",
        "homeboy::core::agent_task_aggregate",
        "homeboy::core::agent_task_cook_loop",
        "homeboy::core::agent_task_fanout",
        "homeboy::core::agent_task_finalization",
        "homeboy::core::agent_task_gate",
        "homeboy::core::agent_task_lifecycle",
        "homeboy::core::agent_task_loop_controller",
        "homeboy::core::agent_task_promotion",
        "homeboy::core::agent_task_provider",
        "homeboy::core::agent_task_schedule",
        "homeboy::core::agent_task_scheduler",
        "homeboy::core::agent_task_secrets",
        "homeboy::core::agent_task_service",
        // runner implementation module
        "homeboy::core::runner::",
        "homeboy::core::runner ",
        "use homeboy::core::runner;",
        "use homeboy::core::runner as ",
        // artifact implementation files
        "homeboy::core::artifact_inputs",
        "homeboy::core::artifact_links",
        "homeboy::core::artifact_manifest",
        "homeboy::core::artifact_metadata",
        "homeboy::core::artifact_origin",
        "homeboy::core::browser_evidence",
        "homeboy::core::change_artifact",
        "homeboy::core::publication_artifacts",
        "homeboy::core::structured_sidecar",
    ];

    let mut violations = Vec::new();
    for relative in scanned_roots {
        scan_command_layer_for_impl_imports(
            root,
            &root.join(relative),
            &forbidden,
            &mut violations,
        );
    }

    assert!(
        violations.is_empty(),
        "command layer must depend on explicit core facades only. Replace these imports with \
         `homeboy::core::agent_tasks::*`, `homeboy::core::runners::*`, or \
         `homeboy::core::artifacts::*` (or one of their nested groups). Violations:\n{}",
        violations.join("\n")
    );
}

fn scan_command_layer_for_impl_imports(
    root: &std::path::Path,
    path: &std::path::Path,
    forbidden: &[&str],
    violations: &mut Vec<String>,
) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).expect("read command layer directory") {
            let entry = entry.expect("read command layer entry");
            scan_command_layer_for_impl_imports(root, &entry.path(), forbidden, violations);
        }
        return;
    }

    if path.extension().is_none_or(|extension| extension != "rs") {
        return;
    }

    let relative = relative_source_path(root, path);
    let content = std::fs::read_to_string(path).expect("read command layer source file");
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
fn core_facades_expose_explicit_groups_not_wildcards() {
    // The `core::agent_tasks`, `core::runners`, and `core::artifacts` facade
    // modules are the public surface that the command layer depends on. They
    // must spell out their re-exports explicitly so that implementation
    // modules cannot leak new public names through `pub use module::*` blocks.
    let agent_tasks = source_file("src/core/agent_tasks.rs");
    let runners = source_file("src/core/runners.rs");
    let artifacts = source_file("src/core/artifacts.rs");

    let forbidden_wildcards = [
        "pub use super::agent_task::*",
        "pub use super::agent_task_aggregate::*",
        "pub use super::agent_task_cook_loop::*",
        "pub use super::agent_task_fanout::*",
        "pub use super::agent_task_finalization::*",
        "pub use super::agent_task_gate::*",
        "pub use super::agent_task_lifecycle::*",
        "pub use super::agent_task_loop_controller::*",
        "pub use super::agent_task_promotion::*",
        "pub use super::agent_task_provider::*",
        "pub use super::agent_task_schedule::*",
        "pub use super::agent_task_scheduler::*",
        "pub use super::agent_task_secrets::*",
        "pub use super::agent_task_service::*",
    ];
    for wildcard in forbidden_wildcards {
        assert!(
            !agent_tasks.contains(wildcard),
            "src/core/agent_tasks.rs must not re-export implementation modules via \
             `{wildcard}`; list the API names explicitly so the facade documents its surface."
        );
    }

    assert!(
        !runners.contains("pub use super::runner::*"),
        "src/core/runners.rs must not re-export the runner module via `pub use super::runner::*`; \
         list the API names explicitly so the facade documents its surface."
    );

    let forbidden_artifact_wildcards = [
        "pub use super::artifact_inputs::*",
        "pub use super::artifact_links::*",
        "pub use super::artifact_manifest::*",
        "pub use super::artifact_origin::*",
        "pub use super::browser_evidence::*",
        "pub use super::change_artifact::*",
        "pub use super::publication_artifacts::*",
        "pub use super::structured_sidecar::*",
    ];
    for wildcard in forbidden_artifact_wildcards {
        assert!(
            !artifacts.contains(wildcard),
            "src/core/artifacts.rs must not re-export implementation modules via \
             `{wildcard}`; list the API names explicitly so the facade documents its surface."
        );
    }

    assert!(
        agent_tasks.contains("pub mod scheduler")
            && agent_tasks.contains("pub mod lifecycle")
            && agent_tasks.contains("pub mod service")
            && agent_tasks.contains("pub mod loop_controller"),
        "src/core/agent_tasks.rs must keep the explicit API group modules (scheduler, lifecycle, \
         service, loop_controller, ...) so callers can disambiguate overlapping names."
    );
    assert!(
        runners.contains("pub mod registry")
            && runners.contains("pub mod connection")
            && runners.contains("pub mod execution")
            && runners.contains("pub mod workspace")
            && runners.contains("pub mod lab_offload"),
        "src/core/runners.rs must keep the explicit API group modules (registry, connection, \
         execution, workspace, lab_offload, ...) so callers depend on operation contracts."
    );
    assert!(
        artifacts.contains("pub use super::artifact_links::{")
            && artifacts.contains("PublicArtifactUrlValidation")
            && artifacts.contains("pub use super::artifact_manifest::{")
            && artifacts.contains("ArtifactManifest")
            && artifacts.contains("pub use super::artifact_origin::{")
            && artifacts.contains("ArtifactOriginServeSpec"),
        "src/core/artifacts.rs must keep explicit artifact API re-export groups so callers depend on \
         the facade without wildcard-leaking implementation modules."
    );
}

#[test]
fn architecture_docs_source_paths_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let docs = [
        "docs/architecture.md",
        "docs/developer-guide/architecture-overview.md",
        "docs/developer-guide/architecture-cleanup-map.md",
    ];
    let mut missing = Vec::new();

    for doc in docs {
        let content = source_file(doc);
        for source_path in backtick_source_paths(&content) {
            if !root.join(source_path).exists() {
                missing.push(format!("{doc} references missing `{source_path}`"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "architecture docs must not claim missing source paths:\n{}",
        missing.join("\n")
    );
}

fn backtick_source_paths(content: &str) -> Vec<&str> {
    content
        .split('`')
        .enumerate()
        .filter_map(|(index, segment)| (index % 2 == 1).then_some(segment))
        .filter(|segment| {
            segment.starts_with("src/")
                && !segment.contains("...")
                && segment
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '-' | '.'))
        })
        .collect()
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

    // The detector-agnostic-source policy debt (#2836 / parent #2240) is now
    // fully cleaned: no core detector under `code_audit::detectors` hardcodes an
    // ecosystem literal, so the policy legitimately produces zero findings. The
    // invariant we enforce is therefore the agnosticism itself — no NEW findings
    // and no STALE baseline rows — not a transitional "must still have debt"
    // floor. Reintroducing a hardcoded ecosystem literal fails the NEW-findings
    // assertion below.
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
