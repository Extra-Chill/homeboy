# Set Up Lab Runners

Use Lab runners when Homeboy should execute hot or remote-capable work somewhere other than the controller machine. A runner gives agents and CI a durable execution target with known workspace roots, Homeboy binary identity, environment, secrets, resource policy, and artifact behavior.

## Use This When

- Review, test, benchmark, trace, fuzz, or agent-task work should not run on the controller.
- Release-gate proof must come from a non-local runner path.
- A runner should own workspaces and artifacts for headless automation.
- Provider credentials or runtime tools need to be checked before expensive work starts.

## 1. Create Or Reuse A Server Record

SSH runners are usually enabled on a `homeboy server` record. The common Lab path uses the same id for the server and runner:

```bash
homeboy server create <runner-id> --host <host> --user <user> --port 22
homeboy runner enable <runner-id> --workspace-root <workspace-root> --concurrency-limit 4 --artifact-policy copy
```

Local runners are available for this machine, but they are never auto-selected for Lab offload.

## 2. Connect And Inspect

```bash
homeboy runner connect <runner-id>
homeboy runner status <runner-id>
homeboy runner doctor <runner-id>
```

Use `runner doctor` before serious evidence runs. It checks whether Homeboy, Git, SSH, the workspace root, and declared tools are usable on the runner.

## 3. Configure Preferred Runner Selection

When there is one intended Lab runner, make selection explicit:

```bash
homeboy config set /lab/preferred_runner '"<runner-id>"'
```

Command selection remains conservative:

- `--runner <id>` always wins.
- A preferred SSH runner can be auto-selected for portable hot commands.
- Local runners are not auto-selected.
- If a selected runner cannot connect, explicit `--runner` fails instead of silently falling back.

## 4. Separate Environment From Secrets

Runner config separates printable environment from secret references:

```json
{
  "env": {
    "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL": "https://artifacts.example.test"
  },
  "secret_env": {
    "GITHUB_TOKEN": { "env": "GITHUB_TOKEN" }
  }
}
```

`env` values are diagnostic. `secret_env` values are resolved at execution time and are never printed as secret values.

## 5. Verify Lab-Offload Readiness

Run the Lab-specific diagnostic scope before treating a runner as proof-capable:

```bash
homeboy runner doctor <runner-id> --scope lab-offload
```

Use the narrow repair path only for the specific self-healing checks it owns:

```bash
homeboy runner doctor <runner-id> --scope lab-offload --repair
```

More invasive fixes such as upgrading binaries, refreshing caches, or rewriting runner paths remain explicit operator actions.

## 6. Refresh Runner Homeboy Deliberately

Runner proof depends on the Homeboy binary actually used by the runner:

```bash
homeboy runner refresh-homeboy <runner-id> --ref main --dry-run
homeboy runner refresh-homeboy <runner-id> --ref main --reconnect
homeboy runner status <runner-id>
```

`runner status` reports controller, configured executable, active daemon version, build identity, drift signals, and follow-up refresh commands.

## 7. Run Portable Work Through The Runner

```bash
homeboy --runner <runner-id> review <component-id> --changed-since origin/main
homeboy --runner <runner-id> bench <component-id> --baseline
homeboy --runner <runner-id> trace <component-id> <scenario>
homeboy --runner <runner-id> agent-task controller run-from-spec @controller.json --max-actions 5
```

For runner-side checkouts, use `runner exec`:

```bash
homeboy runner exec <runner-id> \
  --cwd /srv/homeboy/checkouts/<component-id> \
  -- homeboy review <component-id> --changed-since origin/main
```

## 8. Preserve Runner Evidence

Treat runner stdout as operator context. Use command JSON, persisted runs, and artifact promotion for reviewer-facing evidence:

```bash
homeboy --output homeboy-results/review.json \
  --runner <runner-id> review <component-id> --changed-since origin/main

homeboy runs show <run-id>
homeboy runs artifacts <run-id>
```

## Reference

- [runner command](../commands/runner.md)
- [Use runners](use-runners.md)
- [Release-gate proof path](../operations/release-gate-proof-path.md)
- [Artifact loop for runner and matrix workflows](../operations/artifact-loop-runner-matrix.md)
