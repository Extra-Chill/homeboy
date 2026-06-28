// Fixture command-layer file under a `tests/` subdirectory.
//
// PURPOSE: exercise the walker's test-path skipping. This file lives under
// `src/commands/tests/`, so its relative path contains `/tests/` and
// `walker::is_test_path` returns true. The fixture's `thin_command_adapter`
// policy has `skip_test_paths: true`, so this file MUST be skipped even though
// it sits inside the configured `src/commands/` include path and carries the
// configured ORCHESTRATION_MARKER below.
//
// REGRESSION GUARD: if the walker's test-path skipping breaks (the #6906 class
// of bug where test files are wrongly scanned), this file's orchestration
// weight starts producing a `thin_command_adapter_violation` finding. That new
// finding is NOT in EXPECTED_FINDINGS, so the snapshot harness fails — exactly
// the catch that was missing. The dedicated
// `audit_runtime_regression_skips_test_paths` test asserts the same invariant
// directly.
pub fn run_skipped_command() {
    // ORCHESTRATION_MARKER: orchestration weight that MUST be ignored because
    // this file lives under a test path.
    let _ = skipped_orchestrate();
}

fn skipped_orchestrate() -> u32 {
    7
}
