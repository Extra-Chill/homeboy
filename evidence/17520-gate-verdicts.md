# Issue 17520 validation (incomplete)

Local validation:

- `cargo check -p homeboy-cli --locked`: passed.
- `cargo test -p homeboy-agents --lib gate --locked`: 403 passed,
  1 ignored, 2167 filtered out (124-second-scale run).
- After the final changes, both new verdict tests passed again.
- `cargo fmt --all --check` and `git diff --check`: passed.

The behavioral test executes real shell gates in a temporary Git repository.
An agent pass is reused by subsequent verification; an unchanged retry executes
only the failed command. Changing the candidate tree executes the passed command
again. A separate test checks command and environment misses, original-result
links and saved-duration projection. These are local tests, not Lab proof.

## Actual Lab preflight output

Command: `homeboy --placement lab runner list`

```json
{
  "success": false,
  "exit_code": 2,
  "status": "failed",
  "summary": "Invalid argument 'placement': required Lab placement has no selected ready runner (readiness: unknown); controller-side source preparation did not start and no workload executed locally"
}
```

The configured inventory exposed only `local`. No blocks-engine Cook was run,
no retry Cook was demonstrated on Lab, and no real Lab time saving was measured.
The issue's Lab acceptance criterion is not met. Do not describe the local
fixture's duration projection as measured Lab savings.

Remaining integration limits: verdict storage is execution-host-local; ephemeral
Lab data roots cannot share it. The new CLI currently exposes public shell gates,
not adapter-owned test plans. Controller-side admission/materialization/setup
still occurs for Lab gates before the execution-host cache can be checked.
