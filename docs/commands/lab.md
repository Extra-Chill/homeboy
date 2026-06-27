# Lab

Plan Lab-oriented runner workflows without executing the workload.

## Refresh Plan

`homeboy lab refresh-plan` validates the runner configuration and local source
inputs for a matrix-style refresh, then prints the existing Homeboy commands to
run next:

```sh
homeboy lab refresh-plan \
  --runner lab-runner \
  --workspace ./component \
  --runner-cwd /workspace/component \
  --run-id matrix-refresh-20260627 \
  --source ./component/src \
  --fixture ./component/fixtures \
  --output artifacts/matrix \
  --summary artifacts/matrix/matrix-summary.json \
  -- \
  sh -lc './scripts/run-matrix --out artifacts/matrix'
```

The plan is intentionally read-only. It composes the current primitives:

- `homeboy runner doctor <runner> --scope lab-offload`
- `homeboy runner workspace sync <runner> --path <workspace> --mode <mode>`
- `homeboy runner exec <runner> --artifact <path> --summary <path> ...`
- `homeboy runs artifacts <run-id>`
- `homeboy runs evidence <run-id>`

Use the artifact loop guide for the evidence shape expected from runner and
matrix workflows: `docs/operators/artifact-loop-runner-matrix.md`.
