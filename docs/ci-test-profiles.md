# PR and nightly test coverage

`.config/nextest.toml` owns the explicit PR exclusion list. The measurements
come from #16264's full-suite Candidate Test on #16260 (14,701 tests, 791s
wall time). We move cook-handoff, execution-step chaos, and daemon-chaos
lifecycle suites as groups, rather than timing-dependent selection. Their
reported peaks were 44.6s, 38.4s, and 35.5s respectively. Two additional exact
identities measured 35.2s (repository cleanliness) and 31.9s (subtree split).
Other unit-test binaries remain enabled: the issue gives aggregate counts,
not sufficient identities to safely exclude all their >=10s tests.

## Which profile actually runs?

The unsharded `candidate`, `baseline` and retry (`reconcile`) jobs of the
reusable homeboy-action workflow export its `nextest-profile` input as
`NEXTEST_PROFILE` (homeboy-action#546). `ci.yml` sets `nextest-profile: ci`
and `nightly.yml` sets `nextest-profile: nightly`.

Do **not** pass the profile as `review test -- --profile ci`: the Rust runner
rejects passthrough arguments whenever changed-scope selection
(`HOMEBOY_RUST_CHANGED_TEST_SELECTION_FILE`) or shard replay
(`HOMEBOY_TEST_SHARD_MANIFEST`) is active, so every scoped PR would exit 2.
Local nextest's default profile remains unchanged (full coverage).

`.github/workflows/nightly.yml` calls the same reusable workflow at 05:23 UTC
on the default branch, with `scope: full`, one unsharded test run, no
baseline/differential gating, and `nextest-profile: nightly`. Manual dispatch is also
available. `nightly` explicitly selects `all()` and inherits the default
same-child recovery resource/timeout override. The reusable workflow supports
this caller's schedule via `workflow_call`; no separate Rust setup is needed.
If enabling shards later, revisit profile propagation: their hardcoded
`NEXTEST_PROFILE: ci` and exact-identity inventory/preflight need to agree.

## Changed-file trade-off

Nextest intersects explicit identities/filters with `default-filter`; naming
an excluded test does not by itself re-enable it. This PR intentionally keeps
the reusable workflow contract and does not add a second changed-file planner.
Thus even PRs changing excluded tests defer that coverage to nightly. For
pre-merge coverage, manually dispatch Nightly tests on the PR branch, or run
`cargo nextest run --workspace --profile nightly` locally. When invoking a
specific excluded test under `ci`, use `--ignore-default-filter` as well as
its exact identity. A future upstream changed-scope integration should apply
that flag only for explicitly selected identities, not all full-suite PRs.

## Verify selection without running slow tests

```
cargo nextest list --workspace --profile ci --message-format json > /tmp/ci.json
cargo nextest list --workspace --profile nightly --message-format json > /tmp/nightly.json
python3 scripts/check-nextest-profiles.py /tmp/ci.json /tmp/nightly.json
```

The checker verifies every excluded group exists, is filtered out of CI,
returns in nightly, and that all other test identities retain selection.
