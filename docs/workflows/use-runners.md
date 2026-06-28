# Use Runners

Runners let Homeboy route hot or remote-capable work away from the controller machine while preserving the same command contract and evidence shape.

## Use This When

- A command is resource-heavy and should not run locally.
- You need release-gate proof from a runner instead of a warm developer machine.
- A task already has a checkout on a runner-side workspace.
- You need runner artifacts to be persisted and inspectable later.

## 1. Check Runner Health

Start by confirming the runner exists and can execute Homeboy commands:

```bash
homeboy runner list
homeboy runner status <runner-id>
homeboy runner doctor <runner-id>
```

Use `status` for identity/readiness. Use `doctor` when the runner is stale, missing a binary, missing environment, or refusing handoff.

## 2. Let Homeboy Route Portable Gates

When a component declares a default runner or Homeboy can infer one, run the normal command and let routing happen:

```bash
homeboy review my-component --changed-since origin/main
```

When you need to pin the runner, pass it globally:

```bash
homeboy --runner <runner-id> review my-component --changed-since origin/main
```

Use the same pattern for runner-capable evidence commands:

```bash
homeboy --runner <runner-id> bench my-component --baseline
homeboy --runner <runner-id> trace my-component checkout-flow --baseline
```

## 3. Execute From A Runner-Side Checkout

When the intended checkout already exists on the runner, dispatch from that checkout instead of forcing controller-local execution:

```bash
homeboy runner exec <runner-id> \
  --cwd /srv/homeboy/checkouts/my-component \
  -- homeboy review my-component --changed-since origin/main
```

This keeps the proof tied to the runner-side workspace.

## 4. Preserve Artifacts

Runner stdout is not enough for durable evidence. Prefer command JSON, persisted runs, or promoted artifact directories:

```bash
homeboy --output homeboy-results/review.json \
  --runner <runner-id> review my-component --changed-since origin/main

homeboy runs show <run-id>
homeboy runs artifacts <run-id>
```

For matrix and static-output workflows, follow [Artifact loop for runner and matrix workflows](../operations/artifact-loop-runner-matrix.md).

## 5. Repair Before Bypassing

If runner routing fails because the runner is stale, missing secrets, or has a dirty workspace, fix the runner state before claiming proof. Local-hot bypass flags are debugging aids, not release-gate proof.

Use the release-gate runbook for the stricter proof boundary: [Release-gate proof path](../operations/release-gate-proof-path.md).

## Reference

- [runner command](../commands/runner.md)
- [Release-gate proof path](../operations/release-gate-proof-path.md)
- [Controller to runner reverse-runner setup](../operations/controller-runner-reverse-runner.md)
- [Capture evidence](capture-evidence.md)
