# #18339: Lab gate timestamp diagnosis

## Reproduction

Measured on `homeboy-lab` (Linux x86_64, Cargo 1.95.0). Two fresh directories
were populated from the same tracked Homeboy candidate. Each gate used its own
seeded target; `debug/incremental` was removed from the seed, matching managed
Cargo target projection. The narrow workload was `cargo check -p homeboy-error`
with `CARGO_LOG=cargo::core::compiler::fingerprint=info`.

The second fresh workspace reported:

```text
stale: changed ".../gate2/crates/homeboy-error/src/lib.rs"
(vs) ".../target2/debug/.fingerprint/homeboy-error-a81a541560c090e3/dep-lib-homeboy_error"
FileTime { seconds: 1794902569, nanos: 614318257 } < FileTime { seconds: 1794902604, nanos: 620976337 }
dirty: FsStatusOutdated(StaleItem(ChangedFile { ... }))
```

Preserving source timestamps alone was insufficient when target artifacts were
copied without their timestamps. A separate probe reported:

```text
dependency on `build_script_build` is newer than we are
1794902661.170851812s > 1794902661.166042632s
.../serde-1.0.228
dirty: FsStatusOutdated(StaleDepFingerprint { unit: UnitIndex(9) })
```

The crate metadata suffix stayed `a81a541560c090e3` across workspace paths.
Preserving both source and target timestamps eliminated workspace recompilation
with incremental state still absent. Missing incremental state is therefore not
the trigger for this workload; neither is the workspace directory name.

## Controlled before/after

Both observations used the same initial compiled target and tracked candidate,
a fresh workspace, and an independent copied target without incremental state.
Before reproduced fresh source mtimes and ordinary target copies; after retained
source mtimes and used timestamp-preserving target copies.

| second gate | growth_bytes | cpu_user_ms |
| --- | ---: | ---: |
| before | 3,883,479 | 10,623 |
| after | 0 | 106 |

These fields use #18343's definitions: nonnegative target file-size delta and
`getrusage(RUSAGE_CHILDREN)` user-time delta around the waited-for command.
After fingerprint output contained only `Finished ... in 0.12s`, with no dirty
units or `Checking homeboy-error`. These are controlled narrow Lab measurements,
not a completed full Cook cohort or a prediction for different crate/features.
Homeboy's production gates must supply the full-Cook telemetry comparison.

## Contract

Source mtimes travel separately from content identity, per manifest path (not
per blob: identical bytes can have different timestamps). Every execution view
still owns its own file inode. Blob eviction timestamps remain runner-local and
are not used as source timestamps. Old requests without timestamp metadata
retain their previous behavior. Cargo cache copies retain artifact timestamps;
publication links already preserve them. No concurrent gates share a mutable
Cargo target, and incremental state remains excluded from shared publication.
