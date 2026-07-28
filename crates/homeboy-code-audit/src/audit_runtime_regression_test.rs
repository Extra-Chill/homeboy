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

use crate::{
    run_main_audit_workflow, AuditProfile, AuditRunWorkflowArgs, AuditRunWorkflowResult, Finding,
};
use homeboy_core::engine::baseline::BaselineFlags;

/// Component id declared in the fixture's `homeboy.json`.
const FIXTURE_COMPONENT_ID: &str = "audit-runtime-fixture";

/// Absolute path to the fixture component tree, derived from the crate root so
/// the test is independent of the current working directory.
///
/// The fixture data lives at the repository-root `tests/fixtures/` tree (shared
/// with the other `tests/core/**` harnesses that are `#[path]`-included into
/// this crate), so we walk up from `crates/homeboy-core` (`CARGO_MANIFEST_DIR`)
/// to the workspace root before descending into `tests/fixtures/audit_runtime`.
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
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
/// result. Panics if the audit cannot run at all.
///
/// Only the `slow-tests` snapshot/determinism pair uses this strict form; the
/// default-tier tests go through [`fixture_audit_or_skip`], so this is gated to
/// avoid a dead-code warning in the default build.
#[cfg(feature = "slow-tests")]
fn run_fixture_audit() -> AuditRunWorkflowResult {
    // Audit resolves the component under audit (here, the portable fixture's
    // homeboy.json) through a provider the CLI registers at startup; a core lib
    // test never runs the CLI, so register the component-backed provider so the
    // fixture's audit config is discovered.
    homeboy_core::component::audit_provider::register();
    run_main_audit_workflow(fixture_workflow_args()).expect(
        "audit workflow runs on the fixture tree \
         (requires an installed extension that fingerprints the fixture's source language)",
    )
}

/// `Some(workflow)` when the audit had a corpus; `None` when no installed
/// extension can fingerprint the fixture's source language.
///
/// Before #10557, that second case produced `files_scanned: 0, findings: [],
/// passed: true` — so every finding assertion below passed VACUOUSLY on a
/// machine with no extension installed. `review audit` now reports an empty
/// corpus as a hard error instead, which is the honest answer, so tests that
/// assert on findings have to distinguish "the audit ran and found nothing"
/// from "the audit could not run". Skipping loudly is that distinction; it is
/// not a weakening, because the vacuous pass was never evidence of anything.
fn fixture_audit_or_skip() -> Option<AuditRunWorkflowResult> {
    homeboy_core::component::audit_provider::register();
    match run_main_audit_workflow(fixture_workflow_args()) {
        Ok(workflow) => Some(workflow),
        Err(error) if error.to_string().contains("audit scanned 0 files") => {
            eprintln!(
                "SKIP: no installed extension can fingerprint the audit_runtime fixture ({error})"
            );
            None
        }
        Err(error) => panic!("audit workflow failed on the fixture tree: {error}"),
    }
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
    // #10558: `src/commands/mod.rs` is an INDEX file and
    // `src/commands/thick_adapter.rs` is the only non-index file in its
    // directory (a singleton group). Both are excluded from convention
    // discovery — correctly — and both must still be scanned by source
    // policies. These two entries are the regression guard.
    "source_policy_violation::src/commands/mod.rs",
    "source_policy_violation::src/commands/thick_adapter.rs",
    "source_policy_violation::src/policy_violation.rs",
    "thin_command_adapter_violation::src/commands/thick_adapter.rs",
    "unreferenced_export::src/boundary_leak.rs",
    "unreferenced_export::src/god_file.rs",
    "unreferenced_export::src/policy_violation.rs",
];

/// The two corpus entries above, isolated so the #10558 property has a test of
/// its own that does not require the slow tier.
const SOURCE_POLICY_CORPUS_GUARDS: &[&str] = &[
    "source_policy_violation::src/commands/mod.rs",
    "source_policy_violation::src/commands/thick_adapter.rs",
];

// Runs the full audit workflow against the fixture tree, which requires a
// fingerprinting extension for the fixture's source language to be installed in
// the resolved config home. That makes it a broad-machinery / real-checkout
// test in the sense of `docs/internals/test-tiers.md`, so it lives in the slow
// tier next to `audit_runtime_regression_is_deterministic` rather than the
// hermetic default gate.
#[cfg(feature = "slow-tests")]
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

#[cfg(feature = "slow-tests")]
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
    let Some(workflow) = fixture_audit_or_skip() else {
        return;
    };

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
    let Some(workflow) = fixture_audit_or_skip() else {
        return;
    };
    let workflow: AuditRunWorkflowResult = workflow;
    let _: &Vec<crate::Finding> = &workflow.findings;
    assert!(
        workflow.exit_code == 0 || workflow.exit_code == 1,
        "workflow must produce a normal audit exit code"
    );
}

/// #10558 regression: source policies must see the files convention discovery
/// legitimately drops.
///
/// Two filters exist for convention SIBLING detection and used to leak into the
/// source-policy corpus because that corpus was built from `discovery.groups`:
///
///   1. index files (`mod.rs`, `lib.rs`, `main.rs`, `index.*`, `__init__.py`),
///      excluded because they organize other files rather than being peers, and
///   2. groups with fewer than two members, dropped because a convention needs
///      peers to exist.
///
/// Neither has any meaning for a term scan. On homeboy itself the pair made 264
/// of 1819 `.rs` files (14.5%) unscannable by ANY source policy — every
/// `mod.rs`, `lib.rs`, `main.rs`, and every `build.rs` alone in a crate root.
///
/// The fixture exercises both in one directory: `src/commands/mod.rs` is an
/// index file, `src/commands/thick_adapter.rs` is the sole non-index file in
/// that directory (a singleton group). Both carry the fixture's configured
/// forbidden term, so both must produce a `source_policy_violation`.
///
/// This lives in the default tier (not `slow-tests`) because it is the property
/// #10558 is about, and it skips — loudly — rather than passing vacuously when
/// no extension can fingerprint the fixture.
#[test]
fn source_policies_scan_index_files_and_singleton_directories() {
    let Some(workflow) = fixture_audit_or_skip() else {
        return;
    };

    let actual = finding_fingerprints(&workflow.findings);
    for guard in SOURCE_POLICY_CORPUS_GUARDS {
        assert!(
            actual.iter().any(|finding| finding == guard),
            "source-policy corpus regressed to the convention corpus: expected `{guard}`.\n\
             `mod.rs` (index file) and the sole file in a directory (singleton group) are dropped \n\
             by convention discovery for reasons that do not apply to a term scan (#10558).\n\
             actual = {actual:#?}"
        );
    }
}

/// The corpus a source policy scans must be a strict superset of the convention
/// corpus, never the other way round.
///
/// Stated as a property rather than a file list so a future filter added to
/// convention discovery cannot silently narrow policy coverage again.
#[test]
fn source_policy_corpus_is_not_narrowed_by_convention_filters() {
    let Some(workflow) = fixture_audit_or_skip() else {
        return;
    };

    let scanned_by_policy: Vec<String> = workflow
        .findings
        .iter()
        .filter(|finding| {
            super::super::findings::finding_kind_key(&finding.kind) == "source_policy_violation"
        })
        .map(|finding| finding.file.replace('\\', "/"))
        .collect();

    // `src/policy_violation.rs` sits in a multi-file directory, so it is in the
    // convention corpus too. The two guards above are NOT. Seeing all three
    // proves the policy corpus is the union, not the intersection.
    for expected in [
        "src/policy_violation.rs",
        "src/commands/mod.rs",
        "src/commands/thick_adapter.rs",
    ] {
        assert!(
            scanned_by_policy.iter().any(|file| file == expected),
            "source policy did not reach {expected}; scanned = {scanned_by_policy:#?}"
        );
    }
}
