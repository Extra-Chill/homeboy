# Runner liveness observation — #15565

## Root cause and consolidation

A direct daemon health request completed in 716ms with a matching recorded lease
and PID. The existing two-second session-liveness operation divided its remaining
budget by the remaining retry count and then by two. Each request could time out
before that healthy response arrived.

Session selection also probed health and returned only the session. Full status,
persisted status, and indexed status then re-probed the selected session, allowing
one observation to report connected and a second observation to overwrite it after
the shared deadline was nearly spent.

The change removes fractional retry budgeting and the redundant state probe.
TCP readiness and HTTP health share the remaining attempt deadline; attempts share
the original operation deadline and retain the three-attempt cap. Session selection
returns the state it actually observed together with its selected session.

## Failing-before proof

Baseline: `5f441c60aaa0a1402154e8534765ca190302b8b8`.

The real loopback HTTP regression delays a matching lease/PID response by 700ms
inside a two-second liveness budget:

```sh
CARGO_BUILD_JOBS=2 cargo test -p homeboy-lab-runner \
  session_health_accepts_a_delayed_endpoint_within_the_liveness_deadline -- --nocapture
```

With only the regression added, the baseline rejected the healthy endpoint:
0 passed, 1 failed, 1.59s test runtime.

## Candidate verification

- `cargo fmt --all -- --check` passed.
- Session-store tests: 35 passed. Includes delayed real HTTP health, a stalled
  endpoint bounded by a 200ms deadline, persisted session/state observation,
  lease/PID mismatch, PID reuse, peer ownership, and retry limits.
- Connection session tests: 97 passed.
- `CARGO_BUILD_JOBS=2 cargo build --bin homeboy` passed.
- Candidate `runner status` on the actual configured runner reported connected,
  fresh, and one live job. It retained the independent controller/daemon version
  mismatch as an admission blocker. This was observation-only verification.
- Complete connection filter: 269 passed, 2 failed on macOS. The two service
  fixtures execute GNU-style `sed -i` and fail with BSD sed. Both failures were
  reproduced on the untouched baseline worktree: its service filter had
  11 passed, 2 failed. The full filter is not claimed green.

Commands for the broader filters:

```sh
CARGO_BUILD_JOBS=2 cargo test -p homeboy-lab-runner connection::session_store::tests -- --nocapture
CARGO_BUILD_JOBS=2 cargo test -p homeboy-lab-runner connection::tests::session -- --nocapture
CARGO_BUILD_JOBS=2 cargo test -p homeboy-lab-runner connection:: -- --test-threads=4
```

## Evidence boundary

This repairs liveness budgeting and repeated session-state observation. It does
not establish that every historical continuation failure has this cause; #15565
also tracks admission replay diagnostics and missing worktree records separately.

Chris authorized direct implementation after the tracked Cook provider and Lab
transport recovery failed. OpenAI gpt-6.1-sol through OpenCode implemented and
verified this slice outside Homeboy coding orchestration. Session:
`ses_eee2462e1ffeg4Fq1ksL2l6F3F`. Finalization is outside Homeboy; deterministic
verification and baseline attribution are recorded above.
