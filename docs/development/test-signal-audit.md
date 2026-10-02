# Test Signal Audit

Concrete pruning associated with [#14833](https://github.com/Extra-Chill/homeboy/issues/14833).
The broader detector proposal remains open.

- Base: `66d27ed35d828cae716240f23395eb7e8c2ee34f`.
- Scope: root integration tests, inline/crate tests, ignored-test annotations,
  and CI selection. Source scans and the existing test-quality detector generated
  candidates; helper assertions, parent subprocess invocation, and historical
  contract changes were checked before removal.
- Result: **six tests removed** — three discarded-result placeholders, two
  duplicates, and one obsolete skipped test. Production code is unchanged.
- This is a source-confirmed candidate audit, not proof that every remaining
  workspace test has unique coverage or meaningful assertions.

## Removed tests and evidence

### Discarded-result placeholders

`crates/homeboy-core/src/server/auth.rs`:

| Test | Reason |
| --- | --- |
| `test_login` | Calls `login` with empty credentials and discards its result. |
| `test_logout` | Calls `logout` and discards its result. |
| `test_status` | Calls `status` and discards its result. |

All three were ignored and passed for any returned success or error. An
unexpected panic could fail them, but they asserted no intended auth behavior.
Deterministic assertions on login/logout/status orchestration remain a coverage
gap; these placeholders did not fill it.

### Duplicate keychain coverage

`crates/homeboy-core/src/keychain.rs`:

| Removed test | Surviving coverage |
| --- | --- |
| `test_set` | `test_get` checks the same set/read round trip and additionally checks a missing read. |
| `stores_reads_and_removes_keychain_value` | `test_get` checks store/read; `test_remove` checks removal followed by a missing read. |

The retained `test_exists` and `test_remove_many` exercise distinct contracts.
Auth wrapper tests remain: they assert result mapping and redaction, rather than
duplicating the raw keychain API.

### Obsolete Composer integration skip

`tests/deps_test.rs::update_with_constraint_changes_manifest_and_lock_for_local_path_package`
asserted automatic synthesis of
`composer require fixture/package:1.1.0 --with-dependencies --no-interaction`.
The built-in `ComposerDependencyProvider`, `ComposerAction`, and
`composer_command_args` implementing that contract were removed in
[`19d393bb6`, #7465](https://github.com/Extra-Chill/homeboy/pull/7465).
Current core dispatches explicitly declared dependency providers; the fixture
declared no update adapter of its own and depended on installed host adapters.

Running the ignored test with real Composer/PHP reproduced an update failure on
both the untouched baseline and the candidate: the selected adapter ran
`composer update`, not the deleted `composer require` synthesis. The test was
also capable of silently passing when Composer was missing. Its old skip reason
claimed real manifests were modified even though it used temporary fixtures.
Removal is justified by the retired contract, not merely the failing result.

Surviving coverage includes
`neutral_adapter_manifest_discovers_install_command_and_runs_update_install`
for explicit package/constraint argument forwarding, the script-based update
and install/rebuild tests, and the adapter lockfile metadata tests added in
[#14999](https://github.com/Extra-Chill/homeboy/pull/14999).
Real package-manager mutation belongs to the adapter's owning integration suite.

## Retained skips

- Six issue-linked quarantines remain. [#15007](https://github.com/Extra-Chill/homeboy/issues/15007)
  describes retry policy overridden by patch preservation; [#15010](https://github.com/Extra-Chill/homeboy/issues/15010)
  describes a reverse-worker fixture without an origin remote.
- The release watchdog test remains ignored under
  [#14880](https://github.com/Extra-Chill/homeboy/issues/14880). It races real work
  against a two-second deadline. Its intended child-kill/JSON-envelope/exit-124
  contract still needs a deterministic slow fixture; deleting it would conceal
  that gap.
- Three annotations still reference closed [#14984](https://github.com/Extra-Chill/homeboy/issues/14984):
  reverse-broker execution receipts, cgroup OOM observation, and a disk-capacity
  floor. Their annotations identify no successor issue. This audit did not
  establish that those host/integration prerequisites are resolved.
- Fifteen meaningful opt-in live-service/runtime tests remain, including auth
  wrapper, keychain, auth-profile, PHP-extension, runtime-package, and live-upgrade
  coverage. Their prerequisites were not exercised against operator credentials.
- Thirty-two ignored subprocess fixtures remain in `cook_tests` (7),
  `runtime_promotion` (7), `generation_store` (6), `generation_retirement_tests`
  (3), upgrade execution (3), command supervision (2), default-branch discovery
  (2), component inventory (1), and the external resolver fixture (1).
  Parent tests invoke them explicitly; their ignore attributes are an isolation
  mechanism.

## Detector limitations

The existing `test_quality` detector returned 27 candidates. Manual review
retained them as meaningful tests or intentional fixtures:

- Six "unreachable nested test" findings were false positives from brace
  counting inside literals. The affected owned-args and symlinked-workload tests
  were discovered by the harness and passed.
- Other vacuity findings overlooked helper assertions, product re-exports,
  exit-status/panic contracts, or side-effect assertions. These findings should
  be review candidates rather than automatic deletion instructions.

## Verification and finalization

Provider checks passed: 12 selected core auth/keychain tests, 14 dependency
integration tests, seven owned-args tests, one symlinked-workload test, and
`cargo fmt --all --check`.

Recovery verification used the final candidate: a Homeboy filtered review
reported 15 passing tests (auth/keychain filter across workspace members), and
the dependency target reported 14 passing tests. Before pruning the obsolete
Composer test, explicit ignored execution failed on both baseline and candidate
with the same adapter update error. Reproduction:

```sh
cargo nextest run --profile quick --test deps_test --run-ignored only
```

Evidence runs: `audit14833-final-deps` (14 passing ordinary tests, then the
pre-existing ignored failure) and `audit14833-baseline-composer` (same failure on
the baseline). Final `audit14833-post-prune-deps` ran
`cargo nextest run --profile quick --test deps_test`: **14 passed, zero skipped**.
Its `final-deps.log` is attached as a persisted Homeboy artifact. Final
`cargo fmt --all --check` and `git diff --check` also passed.

Cook lineage: `retry-7c6f1c40-8bdc-5e37-985f-e713ecdeeb90`; provider attempt:
`retry-7c6f1c40-8bdc-5e37-985f-e713ecdeeb90-attempt-1-79d878ed`.
Cook produced and harvested the patch, but required gate setup failed at
`Rust gate cache hydration phase install_toolchain`. The isolated copied
`rustup` did not respond even to `--version`; the byte-identical host executable
responded normally. Lab promotion recovery rejected runner readiness admission.
These infrastructure failures are not passing gates.

The operator explicitly authorized manual verification and draft-PR finalization
outside Homeboy. OpenCode with `zai-coding-plan/glm-5.3-flash` produced the initial
audit; OpenCode with `openai/gpt-6.1-sol` reviewed the evidence, corrected the
report, verified the retired Composer contract, and finalized the patch.
Scoped verification and authoritative PR CI are reported separately from the
failed Cook gate.
