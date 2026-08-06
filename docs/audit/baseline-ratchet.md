# Audit Baseline Ratchet

`homeboy.json` carries a permanent suppression list for `homeboy audit` at
`baselines.audit.known_fingerprints`. This document explains what is in it, why
it may only shrink, and how to change the ceiling that enforces that.

The ratchet is enforced by `tests/audit_baseline_ratchet_test.rs`.

## What the baseline is

When `homeboy audit --update-baseline` runs, every finding of the current run is
reduced to a fingerprint string and stored. On later runs,
`homeboy_engine_primitives::baseline::compare` treats any finding whose
fingerprint is already in that list as *not new*. Only findings absent from the
list set `drift_increased` and fail the audit.

The list is therefore not a record of history. It is live configuration that
decides what the audit is allowed to report.

## A row is not one finding

This is the part worth internalizing before adding a row.

`AuditFinding::fingerprint` (in `crates/homeboy-code-audit/src/baseline.rs`) builds:

```
convention::file::Kind
```

The description is deliberately excluded, because structural findings embed
volatile values — the test `fingerprint_ignores_description` pins that a
`GodFile` finding keeps one identity as the file grows from 2,484 to 2,645
lines. No fingerprint contains a line number.

Matching is then plain set membership on that string.

The consequence: **baselining is per file + kind, not per instance.** One row
suppresses every finding of that kind in that file — the ones that existed when
the row was written, and every one added afterward. A file baselined for
`IntraMethodDuplicate` is silent on that detector forever, no matter how many
new duplicated blocks land in it.

`CoreBoundaryLeak` is the only exception. It appends a description whose line
numbers are normalized (`... at line <line>`), giving:

```
convention::file::description::Kind
```

so it is per file + kind + message. Still not per instance.

Of the current rows, 821 are the three-segment per-file+kind form (covering 821
distinct file+kind pairs across 582 files) and 312 are the four-segment
`CoreBoundaryLeak` form.

## Two ways the list grows on its own

1. `homeboy audit --update-baseline` re-saves whatever the current run found.
2. A conflicted `homeboy.json` is resolved by `baseline_merge` to the **union**
   of both sides' fingerprints, on the reasoning that each side accepted some
   debt.

Neither path asks anyone to justify a new row, and nothing ages rows out. That
is why the size needs an external check.

## The rule

**`baselines.audit.known_fingerprints` may only shrink.**

`tests/audit_baseline_ratchet_test.rs` pins `AUDIT_BASELINE_CEILING` to the
exact current count and fails if the list exceeds it. A second assertion fails
if the list is *below* the ceiling, which forces the ceiling down as debt is
paid — a ceiling with slack in it is room for silent re-growth.

### Adding a suppression

In order of preference:

1. Fix the finding. This is the default and usually the cheaper option.
2. Retire an existing suppression in the same PR, so the total does not rise.
3. Raise `AUDIT_BASELINE_CEILING` in the same PR, and justify it in the PR body.
   Expect to be asked why the finding cannot be fixed.

### Lowering the ceiling

When suppressions are retired, set `AUDIT_BASELINE_CEILING` to the new exact
count in the same PR. Get the number with:

```sh
jq '.baselines.audit.known_fingerprints | length' homeboy.json
```

The `audit_baseline_ceiling_leaves_no_slack` test will tell you the number if
you forget.

## Current debt, by finding kind

1,133 rows as of the commit that introduced this ratchet. This table exists so
the debt is visible rather than hidden inside a 158 KB JSON blob — that array is
93% of `homeboy.json` by byte count.

| Kind | Entries |
|---|---:|
| `CoreBoundaryLeak` | 312 |
| `SkeletonDuplicate` | 147 |
| `HighItemCount` | 133 |
| `ConstantBypassLiteral` | 128 |
| `IntraMethodDuplicate` | 114 |
| `GodFile` | 94 |
| `UnreferencedExport` | 47 |
| `CommandWrapperBypass` | 37 |
| `NearDuplicate` | 24 |
| `DirectAggregateConstruction` | 15 |
| `ParallelImplementation` | 15 |
| `RemoteExecutionPreflight` | 15 |
| `MissingMethod` | 8 |
| `DuplicateFunction` | 7 |
| `StaleDocReference` | 5 |
| `UnboundedOutputCapture` | 5 |
| `ThinCommandAdapterViolation` | 4 |
| `VacuousTest` | 4 |
| `DeadCodeMarker` | 3 |
| `RepeatedEnumDispatchContract` | 3 |
| `UnusedParameter` | 3 |
| `DirectorySprawl` | 2 |
| `GlobalEnvMutationGuard` | 2 |
| `LayerOwnershipViolation` | 2 |
| `BrokenDocReference` | 1 |
| `MissingInterface` | 1 |
| `NamingMismatch` | 1 |
| `ParallelRunnerSetup` | 1 |
| **Total** | **1133** |

Roughly: 307 duplication findings, 227 size findings, 47 dead public exports.

Regenerate with:

```sh
jq -r '.baselines.audit.known_fingerprints[] | split("::") | .[-1]' homeboy.json \
  | sort | uniq -c | sort -rn
```

Burning this list down is separate work from the ratchet. The ratchet only stops
it from getting worse.

## Known wrinkles

- **`metadata.item_count` is stale.** It reads 1137 while the array holds 1133.
  `normalize_loaded_baseline` only recomputes `item_count` when a policy section
  enumerates fingerprints, and the sections now use the `fingerprint_prefix`
  form with empty enumerations, so that branch never runs. `compare` uses
  `item_count` for its reported `delta`, so that number is off by four. It does
  not affect `drift_increased`, which is computed from the fingerprint set, so
  suppression behavior is correct. The ratchet measures the array length, not
  `item_count`.
- **Rows are not sorted.** `save` sorts, but 363 of the 1,133 rows are out of
  sorted position, so order is not an invariant and no test pins it.
- **The baseline is not in a separate file.** Splitting it out would make config
  diffs readable, but `BaselineConfig::json_path()` hardcodes `homeboy.json`,
  and so do the git-ref reader (`git show <ref>:homeboy.json`) and the whole
  `baseline_merge` conflict driver. There is no external-baseline-path option
  today, so the split is a real change rather than a config tweak.
