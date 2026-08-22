# Dead-code suppression ratchet

`tests/dead_code_suppression_ratchet_test.rs` pins the number of module-level
`#[allow(dead_code)]` attributes in this repo. It may only go down.

## What is counted

An attribute that silences `dead_code` and is attached to a **module
declaration**:

```rust
#[allow(dead_code, reason = "Recovery helpers serve optional lifecycle paths.")]
pub mod daemon;
```

That single line turns the lint off for `daemon` and everything under it —
15,640 lines in that example. The attribute is counted whether the module is
`pub`, `pub(crate)`, `pub(super)` or private, and whether or not it carries a
`reason`.

## What is not counted

A per-item allow:

```rust
#[allow(
    dead_code,
    reason = "Compatibility level is asserted by tests; production branches on the variant only."
)]
Ready(RuntimeSetCompatibility),
```

This is a different act. It names one item, it survives review as a claim about
that item, and the **next** unused member of the same impl still fails the
build. Several of these were added deliberately during the sweep:

| item | why it stays |
|---|---|
| `windows_pipe_error_is_eof` | called only under `cfg(windows)`, tested on every host |
| `AUTOMATIC_RETENTION_OUT_OF_SCOPE` | partition table asserted against a live enum |
| `RuntimeSetAdmission::Ready` payload | classification asserted by tests, not branched on |
| `ActiveResourceLifecycleLiveness::Terminal` payload | owning run id asserted by prune tests |

Inline `mod name { .. }` blocks are also not counted; they are scoped by their
own braces and are not the pattern this guards.

## Why this exists

Before 2026-08 the repo carried **40** module-level suppressions covering
**599,992 lines — 52% of the tree**. `pub mod commands` in `homeboy-cli` was
216,724 lines behind one attribute.

The cost was not the dead code. It was that `cargo check` reported a clean build
while more than half the codebase was exempt, so "zero warnings" carried no
information. Reasons like *"CLI commands retain optional operator and recovery
workflows"* were unfalsifiable.

Clearing them from `homeboy-lab-runner` (#12866), `homeboy-cli` (#12882,
#12954) and `homeboy-core` (#12912) deleted roughly 8,900 lines. Within hours
of landing, the restored lint caught five dead items that arrived from unrelated
PRs the same day — code that would otherwise have been invisible indefinitely.

Nothing prevented one line putting a subtree back in the dark. This test does.

## When the test fails

**Count went up.** Do one of, in order of preference:

1. Delete the dead code instead of hiding the module.
2. Suppress the single item with a `reason` naming why that item specifically
   is unreachable or is read only by a named test.
3. Remove an existing module suppression in the same PR so the total does not
   rise.
4. Argue for raising `MODULE_SUPPRESSION_CEILING` in the PR body. Expect to be
   asked.

**Count went down.** Lower `MODULE_SUPPRESSION_CEILING` to the new count in the
same PR. A ceiling above the real count is room for new suppressions to be added
later without any test noticing.

**A swept crate regained one.** `homeboy-lab-runner`, `homeboy-cli` and
`homeboy-core` are held at zero by a separate assertion, because the aggregate
ceiling alone would not notice one crate regaining a suppression while another
lost one in the same PR.

## Remaining work

```
homeboy-agents             7    ~114,000 lines   triaged, 42 items classified
homeboy-deploy             3
homeboy-release            1     ~39,700 lines
homeboy-extension          1     ~10,800 lines
homeboy-engine-primitives  1
homeboy-paths              1
tests/ support modules     2    shared scaffolding, partially used per binary
```

## Related

- `docs/audit/baseline-ratchet.md` — the same shape applied to the audit
  suppression baseline in `homeboy.json`.
- `docs/audit/dead-guard.md`
