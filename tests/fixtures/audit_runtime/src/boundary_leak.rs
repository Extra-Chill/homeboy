// Fixture core source file that trips the `core_boundary_leaks` agnostic check.
//
// PURPOSE: exercise the CoreBoundaryLeak / core-agnostic-source detector — the
// same detector whose findings exploded in the #6906 regression. The fixture's
// `core_boundary_leaks` config scans `src/` for the synthetic ecosystem term
// `florpstack` (a made-up token, intentionally NOT a real language/framework
// name, so the snapshot stays stable and the check is deterministic).
//
// The BEHAVIORAL line below contains `florpstack` outside any comment, so the
// detector fires and the finding is captured in EXPECTED_FINDINGS.
//
// The comment-only line further down also names the term, but carries the
// configured `allow_line_contains` marker (`audit-allow-florpstack`), so the
// detector treats it as an explicitly allowed example and does NOT fire. This
// proves the allowlist path is exercised and that a comment-context occurrence
// alone does not generate a finding.
pub fn florpstack_adapter() -> &'static str {
    let florpstack = "ecosystem-leak";
    florpstack
}

// Naming florpstack here is intentional and allowlisted. audit-allow-florpstack
pub fn allowed_reference() -> u32 {
    0
}
