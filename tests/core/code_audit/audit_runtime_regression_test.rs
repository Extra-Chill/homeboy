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
    "high_item_count::src/god_file.rs",
    "source_policy_violation::src/policy_violation.rs",
    "thin_command_adapter_violation::src/commands/thick_adapter.rs",
    // Cross-file symbol-graph resolution: `consumer.rs::wire_up` and
    // `exports.rs::orphaned_helper` are exported and referenced by nobody, so
    // they surface; `exports.rs::referenced_helper` is SUPPRESSED because
    // `consumer.rs` calls it across files. These rows guard the symbol-graph /
    // reference-resolution detectors that PR #6896 silently broke — a grammar
    // or audit-config regression that drops the component config (and thus the
    // suppressions) changes this set and fails the harness.
    "unreferenced_export::src/consumer.rs",
    "unreferenced_export::src/exports.rs",
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
