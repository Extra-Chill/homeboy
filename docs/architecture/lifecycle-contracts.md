# Lifecycle Contracts

Homeboy defines a product-neutral lifecycle contract for workloads that mutate
state and need fuzz-safe cleanup. Core owns the vocabulary and result metadata
shape; extensions own concrete runtime hooks.

## Phases

Lifecycle phases use stable `snake_case` names and are expected to execute in
this order when a runner supports them:

- `prepare`: make the runtime ready for a workload without adding test data.
- `seed`: install deterministic fixture data or corpus inputs.
- `snapshot`: capture a restorable state reference after preparation/seeding.
- `reset`: return state to the captured snapshot before the next case or run.
- `rollback`: recover from a failed or interrupted case/run.
- `teardown`: release leases, temporary resources, and runtime-local state.

## Declaration Shape

Manifests and rig workloads may attach an optional lifecycle declaration:

```json
{
  "lifecycle": {
    "schema": "homeboy/lifecycle-contract/v1",
    "version": 1,
    "phases": [
      {
        "id": "prepare",
        "phase": "prepare",
        "extension_hook": "runtime.prepare",
        "timeout_seconds": 120,
        "required": true
      },
      {
        "id": "snapshot",
        "phase": "snapshot",
        "extension_hook": "runtime.snapshot",
        "required": true
      }
    ]
  }
}
```

`extension_hook` and `command` are declarations only. Product/runtime adapters can
implement those hooks later without Homeboy core learning product-specific reset
or snapshot mechanics.

## Result Metadata

Runners that execute lifecycle phases should record result metadata with snapshot
refs. Bench runners write this under `run_metadata.lifecycle`; fuzz runners write
it under `results.lifecycle` in the `homeboy/fuzz-campaign/v1` campaign object.

```json
{
  "schema": "homeboy/lifecycle-result/v1",
  "version": 1,
  "phases": [
    {
      "id": "snapshot",
      "phase": "snapshot",
      "status": "passed",
      "snapshot_ref": "snapshot-1"
    }
  ],
  "snapshot_refs": [
    {
      "schema": "homeboy/lifecycle-snapshot-ref/v1",
      "id": "snapshot-1",
      "kind": "database",
      "phase_id": "snapshot",
      "artifact_id": "db-snapshot"
    }
  ]
}
```

Snapshot refs are metadata pointers, not implicit filesystem paths. Use
`artifact_id` or the nested `artifact` contract when snapshot bytes are persisted
as Homeboy artifacts; use `locator` only for runner-owned opaque references.
