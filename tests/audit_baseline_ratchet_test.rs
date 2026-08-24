//! Monotonic growth ratchet for the audit suppression baseline in `homeboy.json`.
//!
//! `baselines.audit.known_fingerprints` is a permanent-suppression list for
//! `homeboy audit`. Every row in it silences a finding the repo's own audit
//! engine already produced. Nothing in the loader caps the list, ages rows out,
//! or requires a rationale, and two of the write paths *grow* it by default:
//!
//! * `homeboy audit --update-baseline` re-saves whatever the current run found.
//! * the baseline merge driver resolves a conflicted `homeboy.json` by taking
//!   the **union** of both sides' fingerprints (see `baseline_merge`), on the
//!   reasoning that each side accepted some debt.
//!
//! Union-on-conflict plus save-on-demand is a one-way size ratchet pointing the
//! wrong way. This test points it the other way: the list may only shrink.
//!
//! ## Why entry count is the right thing to pin
//!
//! A row is not one suppressed finding. `AuditFinding::fingerprint` in
//! `homeboy-code-audit::baseline` builds `convention::file::Kind` and
//! deliberately excludes the description, because structural findings embed
//! volatile numbers (`fingerprint_ignores_description` pins exactly that).
//! Matching is then plain set membership on that string, in
//! `homeboy_engine_primitives::baseline::compare`.
//!
//! So one row suppresses *every* finding of that kind in that file, now and
//! forever — including ones written after the row was added. `CoreBoundaryLeak`
//! is the single exception: it appends a line-number-normalized description, so
//! it is per-file+kind+message. Nothing is per-instance; no fingerprint carries
//! a line number.
//!
//! That makes the row count a floor on the suppressed surface, never an
//! overestimate — and makes each added row cheap to add and expensive to notice.
//!
//! ## Changing the ceiling
//!
//! Down, in the same PR that removes the rows. Never up. See
//! `docs/audit/baseline-ratchet.md`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// Maximum permitted size of `baselines.audit.known_fingerprints`.
///
/// This is the exact count as of the commit that introduced this ratchet. It is
/// a ceiling, not a target: lower it whenever suppressions are retired, and
/// never raise it. Raising this number is the one edit this test exists to make
/// someone argue for in review.
const AUDIT_BASELINE_CEILING: usize = 1135;

fn baseline_fingerprints() -> Vec<String> {
    let config: Value =
        serde_json::from_str(include_str!("../homeboy.json")).expect("homeboy.json parses");

    config["baselines"]["audit"]["known_fingerprints"]
        .as_array()
        .expect("baselines.audit.known_fingerprints is an array")
        .iter()
        .map(|row| {
            row.as_str()
                .expect("every baseline fingerprint is a string")
                .to_string()
        })
        .collect()
}

/// Tally rows by the trailing `::Kind` segment of the fingerprint.
///
/// Holds for both serialized shapes: `convention::file::Kind` and the
/// `CoreBoundaryLeak` form `convention::file::description::Kind`.
fn entries_by_kind(fingerprints: &[String]) -> BTreeMap<&str, usize> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for fingerprint in fingerprints {
        let kind = fingerprint.rsplit("::").next().unwrap_or("<malformed>");
        *counts.entry(kind).or_default() += 1;
    }
    counts
}

/// Descending per-kind breakdown, so a failure names where the debt actually is.
fn breakdown_by_kind(fingerprints: &[String]) -> String {
    let mut rows: Vec<(&str, usize)> = entries_by_kind(fingerprints).into_iter().collect();
    rows.sort_by(|(left_kind, left_count), (right_kind, right_count)| {
        right_count.cmp(left_count).then(left_kind.cmp(right_kind))
    });

    rows.iter()
        .map(|(kind, count)| format!("    {count:>5}  {kind}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn audit_baseline_may_only_shrink() {
    let fingerprints = baseline_fingerprints();
    let actual = fingerprints.len();
    let ceiling = AUDIT_BASELINE_CEILING;
    let over = actual.saturating_sub(ceiling);
    let breakdown = breakdown_by_kind(&fingerprints);

    assert!(
        actual <= ceiling,
        r#"
audit suppression baseline grew.

  ceiling: {ceiling}
  actual:  {actual}   (+{over})

`baselines.audit.known_fingerprints` in homeboy.json may only SHRINK.

Each row permanently silences an audit finding. Because a fingerprint is
`convention::file::Kind` and carries no line number, one row silences EVERY
finding of that kind in that file — including ones not written yet.

To land this change, do one of:

  1. Fix the finding instead of baselining it. This is the default.
  2. Retire {over} existing suppression(s) in this same PR, so the total does
     not rise.
  3. If the ceiling is genuinely wrong, raise AUDIT_BASELINE_CEILING in
     tests/audit_baseline_ratchet_test.rs in this same PR and say why in the
     PR body. Expect to be asked.

If you got here from `homeboy audit --update-baseline`, note that it re-saves
every current finding, and that a conflicted homeboy.json resolves to the UNION
of both sides' fingerprints. Both grow this list silently.

Current baseline by finding kind:
{breakdown}

See docs/audit/baseline-ratchet.md
"#
    );
}

#[test]
fn audit_baseline_ceiling_leaves_no_slack() {
    let actual = baseline_fingerprints().len();
    let ceiling = AUDIT_BASELINE_CEILING;
    let under = ceiling.saturating_sub(actual);

    assert!(
        actual >= ceiling,
        r#"
audit suppression baseline shrank — good. Now lock it in.

  ceiling: {ceiling}
  actual:  {actual}   (-{under})

Lower AUDIT_BASELINE_CEILING in tests/audit_baseline_ratchet_test.rs to {actual}
in this same PR.

The gap is not harmless: a ceiling above the real count is room for {under} new
suppression(s) to be added later without any test noticing. A ratchet that keeps
slack is not a ratchet. This assertion is what lets the ceiling be trusted to
mean "the current count".

See docs/audit/baseline-ratchet.md
"#
    );
}

#[test]
fn audit_baseline_rows_are_unique() {
    let fingerprints = baseline_fingerprints();

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for fingerprint in &fingerprints {
        *seen.entry(fingerprint.as_str()).or_default() += 1;
    }

    let duplicate_rows: Vec<String> = seen
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(fingerprint, count)| format!("    {count}x  {fingerprint}"))
        .collect();
    let duplicates = duplicate_rows.join("\n");
    let total = fingerprints.len();

    assert!(
        duplicate_rows.is_empty(),
        r#"
audit baseline contains duplicate fingerprints:

{duplicates}

Suppression is set membership, so a duplicate row suppresses nothing extra. It
only inflates the count this file's ratchet measures, which would let real
suppressions be added under the ceiling by deleting duplicates first.

Baselines written by `homeboy audit` are sorted and deduped, so a duplicate
means homeboy.json was hand-edited or hand-merged.

Note: rows are NOT required to be sorted, and this suite does not check order.
Many of the current {total} rows are out of sorted position.
"#
    );
}

/// A baseline row naming a path that no longer exists is not merely stale — it
/// makes `homeboy review audit` fail closed on every full-tree run.
///
/// `homeboy_code_audit::baseline::validate_fingerprint_paths` enforces this, but
/// only when `requires_full_baseline_path_validation` says so, which excludes
/// `--changed-since`. PR CI runs changed-scope, so PR CI tolerates a stale row
/// forever. The only full-tree consumer is the weekly advisory Audit Debt
/// sweep — a workflow whose failure nobody is paged for.
///
/// So a tree move that leaves a row behind lands green, and the full-tree debt
/// discovery it silently broke stays broken until someone runs the audit by
/// hand. This test moves that detection into PR CI, where it costs microseconds.
///
/// It calls the production validator instead of restating the rule, so the
/// definition of "stale" cannot drift between the gate and its guard.
#[test]
fn audit_baseline_rows_reference_live_paths() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let baseline = homeboy_code_audit::baseline::load_baseline(repo_root)
        .expect("homeboy.json carries an audit baseline");

    if let Err(error) =
        homeboy_code_audit::baseline::validate_fingerprint_paths(repo_root, &baseline)
    {
        panic!(
            r#"
audit baseline references paths that no longer exist.

{error}

A full-tree `homeboy review audit` FAILS on this — it does not merely warn.
`--changed-since` runs skip the check, so PR CI and rolling releases stay green
while the weekly Audit Debt sweep is dead.

Fix: repoint the row at the file's new path if the finding still applies. If it
does not, remove that exact row with:

  homeboy review audit baseline prune --path . --fingerprint <fingerprint>

Then run `homeboy review audit baseline validate --path .`. Prune refuses an
unmatched fingerprint; inspect its deterministic diff rather than regenerating
or hand-editing the baseline. Do this in the same PR as the move.

See docs/audit/baseline-ratchet.md
"#
        );
    }
}
