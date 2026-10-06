# Cook admission recovery — #15565

## Proven failures and owning repairs

Baseline: `e4d921f07` (includes the runner liveness repair #15579).

1. A real native Git worktree was created, given an additional committed file,
   then its directory was deleted. Provider planning failed with `native worktree
   ... is not safe for reuse`, although `worktree::create` already has an exact
   registration/branch restoration primitive. Missing active native tasks now
   resolve as unmaterialized destinations, allowing that existing primitive to
   restore the original branch, commits, and registry record after admission.
   Preview remains read-only; missing adopted workspaces remain errors.
2. A real exited replay subprocess supplied a typed validation diagnostic to its
   redirected log. Its supervisor persisted only the message: the diagnostic
   code and controller preparation phase were lost, and terminal commands still
   advertised `resume`. The regression failed because `cook_controller_failure`
   was null. The supervisor now carries the full bounded/redacted diagnostic
   into the canonical controller-failure field, terminalizes the exact fenced
   admission atomically, and advertises status/diagnose/retry.
3. Retryable failures and worker-recorded backpressure no longer lose their
   reason or retry time on lease release. An otherwise empty worker log receives
   a typed replay observation including the child exit result and next attempt.

No fuzzy recovery, forced metadata retirement, or branch reset is introduced.
Restoration uses the existing exact Git registration and identity checks.

## Verification

Failing-before reproductions:

```sh
cargo test -p homeboy-core native_provider_restores_missing_worktree_with_its_original_commits_and_record -- --nocapture
cargo test -p homeboy-cli replay_worker_supervisor_terminalizes_a_deterministic_validation_failure -- --nocapture
```

Both failed against the baseline with only their regression assertions added.

Candidate checks:

- `cargo fmt --all -- --check` and `git diff --check` passed.
- Native provider tests: 8 passed, including restoration without losing an
  unpublished commit, repeated admission, exact registry identity, and rejection
  of missing adopted workspaces.
- Existing native worktree suite: 82 passed, covering locked/foreign
  registrations, branch ownership, relative pointers, dirty/unpushed state,
  live owners, removal authority, and restoration safety.
- Replay-focused CLI filter: 62 passed.
- Complete dispatch filter with nextest process isolation: 110 passed.
- Public status/diagnose and deferred-restoration preview regressions: 2 passed.
  They exercise durable records and the actual command projection functions,
  including phase preservation, secret redaction, stale worker fencing, and
  removal of invalid terminal resume guidance.

The plain shared-process Cargo dispatch run timed out at 20 minutes with four
tests waiting. Re-running the same 110-test dispatch filter using the repository's
hermetic nextest runner passed in 9.3 seconds. The timed-out run is not claimed
green; nextest is the completed broader verification.

Representative isolated commands:

```sh
cargo nextest run -p homeboy-core -E 'test(worktree_provider::tests)' --test-threads 4
cargo nextest run -p homeboy-cli -E 'test(commands::infra::route::tests::dispatch)' --test-threads 4
cargo nextest run -p homeboy-cli -E 'test(replay_admission_failure) | test(cook_preview_and_provision)' --test-threads 4
```

## Workflow and provenance

Chris authorized direct Homeboy implementation in this fork. OpenAI gpt-6.1-sol
through OpenCode reproduced, implemented, reviewed, and verified the change in an
isolated tracker-linked worktree. Implementation and finalization occurred
outside Homeboy coding orchestration following the previously recorded
provider/transport failure. Session: `ses_eee2462e1ffeg4Fq1ksL2l6F3F`.
