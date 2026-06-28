//! Runtime audit regression harness.
//!
//! This test runs the **real CI audit entry point**
//! ([`run_main_audit_workflow`]) against a self-contained fixture component tree
//! (`tests/fixtures/audit_runtime/`) and asserts that the produced finding set
//! is byte-for-byte identical to a committed snapshot.
//!
//! WHY THIS PATH (the #6855 fidelity fix): the CI Audit gate and the
//! `homeboy audit` CLI (`src/commands/audit.rs::run`) build an
//! [`AuditRunWorkflowArgs`] and call [`run_main_audit_workflow`] — passing
//! resolved `reference_paths`, a `profile`, and `baseline_flags`. The previous
//! version of this harness instead called the lower-level
//! [`audit_path_with_id`], which reads reference paths from the environment and
//! skips the workflow orchestration (filtering, baseline comparison, scoping,
//! exit-code derivation). That was a DIFFERENT code path from CI. Three #6855
//! Phase 1 attempts (#6896/#6906/#6915) passed this harness yet failed the CI
//! Audit gate, because the regression lived in the
//! reference-path/symbol-graph/workflow machinery that `audit_path_with_id`
//! never exercised. This harness now audits through [`run_main_audit_workflow`]
//! — the exact function CI calls — so a workflow- or reference-path-level
//! regression reproduces locally at `cargo test`.
//!
//! The harness mirrors `src/commands/audit.rs::run` for a self-contained
//! component: `profile = AuditProfile::Full` (the CLI's `--profile` default),
//! `baseline_flags = default` (no `--baseline` / `--ignore-baseline`),
//! `conventions = false`, no kind/label filters, `changed_since = None`,
//! `extension_overrides = []`. Reference paths are EMPTY: the CLI derives them
//! from installed-extension setup scripts (`resolve_audit_reference_paths`), but
//! the fixture declares no extensions and ships its own portable `homeboy.json`,
//! so a faithful self-contained audit has no external reference codebases — the
//! dead-code / symbol-graph detector then sees only the fixture's own files,
//! exactly as CI would for a component without reference setup.
//!
//! The fixture directory ships its own `homeboy.json` portable config (with an
//! `id` and an `audit` block), so it is audited with that config — independent
//! of the host's real `homeboy.json` or any `HOMEBOY_*` reference-path env vars.
//!
//! WHY THIS EXISTS: detector, config-schema, grammar, or workflow-orchestration
//! changes can silently alter audit OUTPUT while still passing `cargo build` and
//! unrelated unit tests. That gap is exactly what let PR #6896 pass Lint+Test
//! while breaking the live audit. This test closes it: any change that alters
//! what the audit emits on a fixed input — through the SAME entry point CI uses
//! — fails here, locally, at `cargo test`.
//!
//! FIXTURE COVERAGE: the fixture tree deliberately exercises the two detector
//! behaviors a real-codebase regression (#6906) slipped past the original
//! harness:
//!   1. TEST-PATH SKIPPING — `src/commands/tests/skipped_helper.rs` carries the
//!      configured orchestration marker but lives under a `/tests/` path, so the
//!      `thin_command_adapter` policy (with `skip_test_paths: true`) must skip
//!      it. Its absence from the snapshot, plus the dedicated
//!      `audit_runtime_regression_skips_test_paths` test, catch a regression
//!      that wrongly scans test files.
//!   2. CORE-AGNOSTIC / CORE-BOUNDARY-LEAK — `src/boundary_leak.rs` contains a
//!      synthetic ecosystem term (`florpstack`) on a behavioral line, firing the
//!      `core_boundary_leaks` detector, while an allowlisted comment occurrence
//!      proves the allow path is honored. This exercises the detector whose
//!      findings exploded in #6906.
//!
//! IF THIS TEST FAILS after a detector/config/grammar/workflow change: inspect
//! the diff between `actual` and `EXPECTED_FINDINGS`. The change altered audit
//! output. Only update the snapshot below if the change is *intentional* — never
//! to make a red test green without understanding what moved.
//!
//! Wired into `src/core/code_audit/entry.rs` via
//! `#[cfg(test)] #[path = ...] mod audit_runtime_regression_test`.

use std::path::PathBuf;

use crate::core::code_audit::{
    self, run_main_audit_workflow, AuditProfile, AuditRunWorkflowArgs, AuditRunWorkflowResult,
    Finding,
};
use crate::core::engine::baseline::BaselineFlags;

/// Component id declared in the fixture's `homeboy.json`.
const FIXTURE_COMPONENT_ID: &str = "audit-runtime-fixture";

/// Absolute path to the fixture component tree, derived from the crate root so
/// the test is independent of the current working directory.
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audit_runtime")
}

/// Build the workflow args exactly as `src/commands/audit.rs::run` does for a
/// self-contained component (no installed extensions, no reference setup, all
/// CLI flags at their defaults). This is the single place the harness mirrors
/// the CI audit entry point — keep it aligned with the command layer.
fn fixture_workflow_args() -> AuditRunWorkflowArgs {
    let root = fixture_root();
    AuditRunWorkflowArgs {
        component_id: FIXTURE_COMPONENT_ID.to_string(),
        source_path: root.to_string_lossy().to_string(),
        // CLI resolves these from installed-extension setup scripts via
        // `resolve_audit_reference_paths`; the fixture declares no extensions,
        // so a faithful self-contained audit has none.
        reference_paths: Vec::new(),
        conventions: false,
        only_kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        only_labels: Vec::new(),
        exclude_labels: Vec::new(),
        profile: AuditProfile::Full,
        extension_overrides: Vec::new(),
        baseline_flags: BaselineFlags::default(),
        changed_since: None,
        precomputed_changed_files: None,
        json_summary: false,
        include_fixability: false,
    }
}

/// Run the fixture audit through the CI entry point and return the workflow
/// result.
fn run_fixture_audit() -> AuditRunWorkflowResult {
    run_main_audit_workflow(fixture_workflow_args())
        .expect("audit workflow runs on the fixture tree")
}

/// Render a finding set into a deterministic, sorted list of compact,
/// volatile-data-free fingerprints (`<kind>::<file>`).
///
/// Line numbers, absolute paths, and counts are intentionally excluded so the
/// snapshot is stable across machines and across non-behavioral refactors. The
/// `file` field is already a path relative to the audited root.
fn finding_fingerprints(findings: &[Finding]) -> Vec<String> {
    let mut rendered: Vec<String> = findings
        .iter()
        .map(|finding| {
            let kind = super::super::findings::finding_kind_key(&finding.kind);
            let file = finding.file.replace('\\', "/");
            format!("{kind}::{file}")
        })
        .collect();
    rendered.sort();
    rendered.dedup();
    rendered
}

/// Committed snapshot of the finding fingerprints the fixture must produce when
/// audited through [`run_main_audit_workflow`] (the CI entry point).
///
/// This list IS the regression guard. See module docs before editing.
const EXPECTED_FINDINGS: &[&str] = &[
    "core_boundary_leak::src/boundary_leak.rs",
    "high_item_count::src/god_file.rs",
    "source_policy_violation::src/policy_violation.rs",
    "thin_command_adapter_violation::src/commands/thick_adapter.rs",
    "unreferenced_export::src/boundary_leak.rs",
    "unreferenced_export::src/god_file.rs",
    "unreferenced_export::src/policy_violation.rs",
];

#[test]
fn audit_runtime_regression_matches_snapshot() {
    let root = fixture_root();
    assert!(root.is_dir(), "fixture root must exist: {}", root.display());

    let workflow = run_fixture_audit();
    let actual = finding_fingerprints(&workflow.findings);
    let expected: Vec<String> = EXPECTED_FINDINGS.iter().map(|s| s.to_string()).collect();

    assert_eq!(
        actual, expected,
        "\nAudit output on the fixture tree changed.\n\
         A detector/config/grammar/workflow change altered what the CI audit entry point emits.\n\
         Inspect the diff; update EXPECTED_FINDINGS only if the change is intentional.\n\
         actual = {actual:#?}\n"
    );
}

#[test]
fn audit_runtime_regression_is_deterministic() {
    let first = finding_fingerprints(&run_fixture_audit().findings);
    let second = finding_fingerprints(&run_fixture_audit().findings);

    assert_eq!(
        first, second,
        "audit output must be deterministic across runs"
    );
}

/// Invariant: the audit must never emit a finding for a file living under a
/// test path (a path segment of `tests/`), because the walker skips test paths.
///
/// This directly encodes the contract that the #6906 regression violated, where
/// test files were wrongly scanned. The fixture ships
/// `src/commands/tests/skipped_helper.rs` — a command-path file carrying the
/// configured `ORCHESTRATION_MARKER`. The `thin_command_adapter` policy has
/// `skip_test_paths: true`, so a healthy walker skips it. If test-path skipping
/// regresses, that file produces a `thin_command_adapter_violation` whose `file`
/// contains `/tests/`, tripping this assertion (and the snapshot test above).
///
/// Because this now runs through [`run_main_audit_workflow`], the file is in
/// scope of the exact pipeline CI uses, so a reference-path/symbol-graph
/// regression that leaks test-path files reproduces here.
#[test]
fn audit_runtime_regression_skips_test_paths() {
    let workflow = run_fixture_audit();

    let test_path_findings: Vec<String> = workflow
        .findings
        .iter()
        .map(|finding| finding.file.replace('\\', "/"))
        .filter(|file| file.starts_with("tests/") || file.contains("/tests/"))
        .collect();

    assert!(
        test_path_findings.is_empty(),
        "audit emitted findings for test-path files (walker test-path skipping regressed): {test_path_findings:#?}"
    );
}

/// Guard that the harness keeps using the CI entry point. `run_main_audit_workflow`
/// must remain reachable here; if it is renamed or its signature changes, this
/// reference (and the harness) must be updated in lockstep with the CLI.
#[test]
fn audit_runtime_regression_uses_ci_workflow_entry_point() {
    // A trivial compile-time + runtime assertion that the workflow result type
    // is what we render from. Keeps the intent explicit: the snapshot above is
    // produced by the same function `src/commands/audit.rs::run` calls.
    let workflow: AuditRunWorkflowResult = run_fixture_audit();
    let _: &Vec<code_audit::Finding> = &workflow.findings;
    assert!(
        workflow.exit_code == 0 || workflow.exit_code == 1,
        "workflow must produce a normal audit exit code"
    );
}
