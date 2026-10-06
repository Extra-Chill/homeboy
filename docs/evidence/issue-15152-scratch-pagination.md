# Issue 15152: bounded scratch inspection

Tracker: https://github.com/Extra-Chill/homeboy/issues/15152

Base: `1fb1fad0a` (`origin/main` at worktree creation).
Branch: `fix/15152-bounded-scratch-inventory`.

## Root cause and change

Scratch cleanup evaluated every indexed resource's lifecycle, Git safety, and
recursive byte accounting before applying its removal limit. Retained resources
could therefore exhaust the category deadline even with `--limit 1`.

The limit now admits resources before those probes. A stable resource-path digest
orders the page and supplies its continuation cursor. Cursor progress survives
earlier deletions and advances past retained rows. Candidate counts and byte
estimates are page-local; uninspected resources are explicit. The old
whole-store `remaining_candidate_count` / `remaining_candidate_bytes` contract
and redundant post-scan candidate slicing are removed. The CLI propagates
partial inventory and the exact continuation command.

The existing ownership, PID, Git, retention, index-lock and destructive-boundary
revalidation checks remain in the admitted-resource path. `--full` expands page
presentation, not resource admission. Detail export commands preserve the
current page cursor. Individual probes retain the category's process-group
deadline; this change bounds resource cardinality, not the cost of an individual
resource's recursive walk or Git probe.

## Failing-before proof

With the added regression test alone on the baseline:

```sh
cargo test -p homeboy-agents --lib scratch_limit_bounds_inspection_before_candidate_accounting -- --nocapture
```

Exit 101: `candidate_count` was **32**, expected **1**. The same test passes on
the candidate and verifies that preview leaves all fixture directories intact.

## Exact candidate verification

All execution ran on Linux in a task-specific source tree using the shared Cargo
target. SHA-256 checks confirmed all six candidate source/document/script files
were byte-identical to the local isolated worktree after formatting.

| Gate | Observed result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo test -p homeboy-agents --lib controller_scratch::tests -- --nocapture` | 47 passed |
| `cargo test -p homeboy-agents --lib replaying_cancel_recovers_stale_released_and_orphaned_scratch_before_bounded_cleanup -- --nocapture` | 1 passed |
| `cargo test -p homeboy-cli --lib commands::cleanup::tests -- --nocapture` | 60 passed |
| `cargo test -p homeboy --test cleanup_deadline_isolation -- --nocapture` | 4 passed |
| `cargo build -p homeboy --bin homeboy` | Passed |
| `python3 scripts/verify-scratch-pagination.py <candidate-binary>` | Passed |
| `git diff --check` | Passed |

The real-binary fixture exercised 65 registered resources with `--limit 5`:

- Preview: 13 pages, 65 inspected, zero removed, 15 protected.
- Apply: 13 pages, 65 inspected, 50 eligible resources removed, all 15 protected
  resources and their evidence retained.
- Every page reported at most five inspections, unique candidate identities,
  and an advancing continuation. Partial category coverage matched the cursor.
- Latest measured maximum page wall time: **13.166 seconds**; total for both
  sweeps: **49.322 seconds**, including isolated CLI/process startup.

Raw gate stdout/stderr is retained in the task-specific session evidence bundle.
The verification transport initially introduced an AppleDouble metadata sidecar;
removing that transport artifact resolved the embedded-doc build failure. The
final gates above all passed afterward.

## Execution and delivery

Implementation and final verification occurred through the operator-authorized
direct OpenCode route for the Homeboy simplification program, outside Cook.
Runtime session: `ses_ef20fd4ccffehC7kK3VTu9uCQN`.
Model: OpenAI `gpt-6.1-sol`; tool: OpenCode. AI implemented the scoped change,
authored the regression and real-binary fixture, and ran/reviewed verification.

This candidate is prepared for PR review. The installed runtime has not been
replaced, and no operator scratch resources were removed by this verification.
