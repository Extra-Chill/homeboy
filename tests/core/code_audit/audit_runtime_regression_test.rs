//! Runtime audit regression harness.
//!
//! This test runs the **full audit pipeline** ([`audit_path_with_id`]) against a
//! self-contained fixture component tree (`tests/fixtures/audit_runtime/`) and
//! asserts that the produced finding set is byte-for-byte identical to a
//! committed snapshot.
//!
//! The fixture directory ships its own `homeboy.json` portable config (with an
//! `id` and an `audit` block), so it is audited with that config — independent
//! of the host's real `homeboy.json` or any `HOMEBOY_*` reference-path env vars.
//!
//! WHY THIS EXISTS: detector, config-schema, or grammar changes can silently
//! alter audit OUTPUT while still passing `cargo build` and unrelated unit
//! tests. That gap is exactly what let PR #6896 pass Lint+Test while breaking
//! the live audit. This test closes it: any change that alters what the audit
//! emits on a fixed input fails here, locally, at `cargo test`.
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
//! IF THIS TEST FAILS after a detector/config/grammar change: inspect the diff
//! between `actual` and `EXPECTED_FINDINGS`. The change altered audit output.
//! Only update the snapshot below if the change is *intentional* — never to
//! make a red test green without understanding what moved.
//!
//! Wired into `src/core/code_audit/entry.rs` via
//! `#[cfg(test)] #[path = ...] mod audit_runtime_regression_test`.

use std::path::PathBuf;

use crate::core::code_audit::audit_path_with_id;

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

/// Render the audit result into a deterministic, sorted list of compact,
/// volatile-data-free fingerprints (`<kind>::<file>`).
///
/// Line numbers, absolute paths, and counts are intentionally excluded so the
/// snapshot is stable across machines and across non-behavioral refactors. The
/// `file` field is already a path relative to the audited root.
fn finding_fingerprints(result: &crate::core::code_audit::CodeAuditResult) -> Vec<String> {
    let mut rendered: Vec<String> = result
        .findings
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

/// Committed snapshot of the finding fingerprints the fixture must produce.
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

    let result = audit_path_with_id(FIXTURE_COMPONENT_ID, &root.to_string_lossy())
        .expect("audit pipeline runs on the fixture tree");

    let actual = finding_fingerprints(&result);
    let expected: Vec<String> = EXPECTED_FINDINGS.iter().map(|s| s.to_string()).collect();

    assert_eq!(
        actual, expected,
        "\nAudit output on the fixture tree changed.\n\
         A detector/config/grammar change altered what the audit emits.\n\
         Inspect the diff; update EXPECTED_FINDINGS only if the change is intentional.\n\
         actual = {actual:#?}\n"
    );
}

#[test]
fn audit_runtime_regression_is_deterministic() {
    let root = fixture_root();
    let path = root.to_string_lossy().to_string();

    let first = finding_fingerprints(
        &audit_path_with_id(FIXTURE_COMPONENT_ID, &path).expect("first audit run"),
    );
    let second = finding_fingerprints(
        &audit_path_with_id(FIXTURE_COMPONENT_ID, &path).expect("second audit run"),
    );

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
#[test]
fn audit_runtime_regression_skips_test_paths() {
    let root = fixture_root();
    let result = audit_path_with_id(FIXTURE_COMPONENT_ID, &root.to_string_lossy())
        .expect("audit pipeline runs on the fixture tree");

    let test_path_findings: Vec<String> = result
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
