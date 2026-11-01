# Issue 16930: public CLI monorepo deployment evidence

The localhost fake-remote deploy harness exercises two configured ZIP components sharing a Git root. The test is `monorepo_outdated_ref_and_receipt_check` in `tests/cli_binary/deploy_phase_timings.rs`.

Observed output:

```text
changed selection: alpha deployed
second invocation: no components selected
project check: alpha and beta receipt digests match; no release download
same-version content update: alpha deployed and receipt-verified only
partial apply: alpha healed and receipt-verified without a recorded SHA
```

The test also asserts exact-ref dry-run selection and its reason, checks receipt provenance for both components, and declares a GitHub release source so receipt-first checks must bypass release resolution. Same-version content updates and remote partial content are verified with the same invocation's `--verify` option.

This is deterministic local CLI evidence, not a production Extra Chill deployment.

## Validation

Gate-fix rerun:
- `cargo fmt --check`: passed.
- `cargo test -p homeboy-deploy -- --test-threads=1`: 362 passed.
- `cargo test -p homeboy-cli -- --test-threads=1`: 2802 passed, 4 ignored.
- `cargo test -p homeboy --test cli_binary`: 64 passed.
- Public CLI end-to-end rerun with `--nocapture`: passed in 9.65 seconds; output above.
- `cargo test -p homeboy --test durable_promotion -- --test-threads=1`: 2 passed.

The gate-fix corrected stale CLI expectations for the removed `--dispatch-provider-id` and review-test `--summary` spellings. The durable-promotion test now inspects the stored observation row at its actual nested lifecycle metadata path: public lifecycle status already resolves externalized payloads. No production behavior was changed to accommodate these tests. The parallel homeboy-cli suite exposed shared-environment interference; its complete serial rerun passed. A full workspace review-gate pass and a production Extra Chill deployment are not claimed.
