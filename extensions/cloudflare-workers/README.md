# Cloudflare Workers Extension

`cloudflare-workers` is Homeboy Extensions' provider-owned deployment command for a generic Cloudflare Worker. It takes one immutable contract file, verifies the clean checkout and Wrangler target, applies declared secrets through stdin, deploys, runs HTTP gates, and rolls back to the deployment recorded before the attempt when secret provisioning, deployment, or a gate fails.

## Contract

Start from [`examples/deploy-contract.json`](examples/deploy-contract.json):

```sh
homeboy extension run cloudflare-workers -- --contract deploy-contract.json
```

The contract requires a clean worktree, exact Git revision, Wrangler config, Worker and account identifiers, expected binding names, secret descriptors, and HTTP gates. Optional `predeploy_commands` run after immutable-source and provider preflight but before secret writes or Worker deployment. Each command declares a safe ID, direct executable, bounded string argument array, optional repository-contained working directory, and optional timeout. Shell executables and working-directory traversal are rejected. Evidence records only command IDs, status, and elapsed time; stdout, stderr, arguments, and paths are omitted. A pre-deploy failure or any resulting source drift stops before Worker mutation.

A secret has exactly one descriptor: an environment variable name or a file path. Values are read only at execution time, written to a temporary `0600` file, supplied to `wrangler secret put` through stdin, and removed in a `finally` cleanup path. They are never command arguments, evidence fields, or raw provider output.

The result is `homeboy/cloudflare-worker-deploy-result/v1`. It records source revision, redacted deterministic stage progress, prior and deployed Worker deployment/version IDs, rollback status, and remediation. Raw Wrangler output and gate bodies are omitted.

Existing targets remain the default. Set `target.create_if_missing` to `true` to explicitly create a new Worker from the immutable source. Homeboy records the bootstrap deployment, provisions secrets only after the target exists, then performs the normal gated deployment. Missing targets without this declaration fail closed.

Gates run once by default. To opt into bounded readiness retries, declare all retry fields on that gate:

```json
{
  "id": "health",
  "url": "https://example.com/health",
  "expected_status": 200,
  "retry": {
    "attempts": 3,
    "retry_delay_ms": 1000,
    "transient_statuses": [500, 502, 503, 504]
  }
}
```

The retry budget includes the first request. Listed unexpected HTTP statuses, timeouts, and network errors use the remaining budget. Text, response-body-limit, and all other status mismatches fail immediately and retain rollback behavior. Gate evidence records each attempt's ordinal, status (HTTP status, `timeout`, or `network_error`), and elapsed milliseconds; it omits response bodies, URLs, headers, and secrets.

The `cli` manifest exposes the generic CLI-compatible surface. Current Homeboy also requires an `executable.runtime.run_command` to run an extension, so this extension supplies that runtime adapter for the same command. The `deployment_providers` descriptor is retained as declarative metadata for a future Homeboy deployment-provider lowering; it is not invoked by current Homeboy releases.

Set `durability.redeploy_same_revision` to redeploy the same checked-out revision after the first successful gate sequence and run the same gates again. Secrets are provisioned once before the first deploy; set `durability.rotate_secrets: true` only when the durability proof explicitly requires a second secret write. This proves the declared external behavior survives a runtime replacement; it does not prove durability of state that the declared gates do not exercise.

## Limits

The first contract targets Wrangler 4.112's `deployments list --json` response and `rollback <version-id>` command. A weighted multi-version deployment is rejected because no version selection policy is in this initial contract. It intentionally does not infer bindings, routes, secret names, or gates from application code. Declare those externally so the same extension can deploy any Worker.
