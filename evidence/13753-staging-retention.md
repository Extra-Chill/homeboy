# Canonical staging retention

Trackers: #13753, #13755.

## Root cause and deletion

Base `7f3d37bb97` already emits `delete_workspace_on_failure`, but duplicates
the entire `LabStagingRecipe` in `LabStagingRecipeWire` to deserialize and invert
the retired `preserve_workspace_on_failure` flag. A separate staging-store pass
rereads raw JSON and rewrites the store solely to migrate that flag.

Delete the wire duplicate, custom deserializer, and `migrate_legacy_retention`
pass. Derive strict deserialization on the same recipe used by the canonical
writer. No current old-field producer was found in Homeboy production source,
Homeboy Action, or Homeboy Extensions.

Production: **1 added / 135 removed, net 134 fewer physical lines**. Tests:
76 added / 60 removed. Command documentation and this evidence are separate.

## Contract and behavior

`delete_workspace_on_failure` is the single boolean retention field. Omission
retains its existing false default. Recipe schema/serialized canonical fields
are unchanged. Old preserve-field records are refused, not migrated or reset;
their exact store bytes remain available. This is an intentional contract break.

Existing admission locking, source authentication, job submission, receipt
publication and intent recovery remain supported. The concurrent-admission
fixture now starts from a canonical submitted intent instead of manufacturing
an obsolete recipe, and proves one materialization and one receipt.

## Verification

Exact source `2ff2998728`, direct Linux Lab, default stack, eight build jobs,
disk-backed temporary directory; capacity reserve gates remain intact:

```sh
cargo fmt --all --check
env -u RUST_MIN_STACK CARGO_BUILD_JOBS=8 cargo nextest run \
  -p homeboy-lab-runner -p homeboy-cli \
  -E 'test(staging) | test(direct_lab_handoff) | test(workspace::tests::prune) | test(lab::offload)' \
  --no-fail-fast --status-level fail --final-status-level fail
git diff --check
```

**395 passed (5 marked leaky), 5,006 skipped**, Nextest
`7256cca8-a0c5-4b8a-805f-db993db80115`. This is scoped verification, not a
full-suite claim. Five passing cases were marked leaky by Nextest; that result
is retained rather than described as a leak-free run.

Real-store tests prove both canonical flag values/default omission survive
restart with no rematerialization or byte rewrite. Retired fields in completed
stages and active intents fail both store opening and admission with no
materialization and unchanged bytes. Unknown recipe fields remain strict.

Reader negative control: base `7f3d37bb97` keeps the original custom recipe
deserializer while receiving the candidate `runner_staging_store.rs` (store
migration deletion plus updated fixtures). The retired-record refusal test
fails because opening the old recipe still succeeds. Nextest
`9bbb737d-f0b2-4123-9ce3-776956ff74d6`: 0 passed, 1 failed, 2,246 skipped.
This isolates recipe-reader retirement; it is not an untouched-base suite run.

## Provenance

Direct implementation/finalization outside Homeboy orchestration is authorized
for Chris Huber's simplification program. Work uses an isolated tracker-linked
Git worktree and direct Lab evidence. OpenAI `openai/gpt-6.1-sol` via OpenCode
traced producers, implemented/reviewed the deletion, updated real-store and
concurrency proof, and ran the recorded checks. Authoritative differential CI
is required before merge.
