# Cloudflare Workers Extension

`cloudflare-workers` is Homeboy Extensions' provider-owned deployment command for a generic Cloudflare Worker. It accepts Homeboy's layered deployment payload or the legacy immutable contract file, verifies the clean checkout and Wrangler target, applies declared secrets through stdin, deploys, runs HTTP gates, and rolls back to the deployment recorded before the attempt when secret provisioning, deployment, or a gate fails.

`homeboy deploy --dry-run` uses the manifest's separate non-mutating provider command. It validates the immutable revision and clean worktree, Wrangler config/account/bindings, authentication, Wrangler's own dry-run, declared secret descriptor availability, and gate declarations. It emits the normal structured result with `mode: "dry_run"` and `status: "validated"`; it does not run predeploy commands or HTTP gates, read secret values, write a result file, provision secrets, deploy, or roll back.

## Contract

The recommended Homeboy contract keeps reusable provider policy in repository-root `homeboy.json`:

```json
{
  "deployment_provider": {
    "extension": "cloudflare-workers",
    "provider": "cloudflare-workers.deploy",
    "policy": {
      "wrangler": { "binary": "wrangler", "config": "wrangler.jsonc", "config_ref": "wrangler.jsonc" },
      "expected_bindings": ["DATABASE", "ASSETS"],
      "predeploy_commands": [],
      "timeout_ms": 120000
    }
  }
}
```

The Homeboy project attachment supplies environment-owned target input:

```json
{
  "deployment_provider_input": {
    "target": { "worker": "production-worker", "account_id": "account-id" },
    "resources": {
      "d1_databases": [{ "binding": "DATABASE", "database_name": "production-database", "database_id": "database-id" }],
      "r2_buckets": [{ "binding": "ASSETS", "bucket_name": "production-assets" }],
      "queues": {
        "producers": [{ "binding": "JOBS", "queue": "production-jobs" }],
        "consumers": [{ "queue": "production-jobs", "dead_letter_queue": "production-jobs-dlq" }]
      }
    },
    "secrets": [{ "name": "API_TOKEN", "env": "PRODUCTION_API_TOKEN" }],
    "secret_inputs": [],
    "gates": [{ "id": "health", "url": "https://worker.example/health", "expected_status": 200 }],
    "durability": { "redeploy_same_revision": true }
  }
}
```

Homeboy materializes `homeboy/deployment-provider-payload/v1` in a private temporary file. The extension accepts Wrangler/config policy, expected binding names, predeploy declarations, and timeout only from `policy.value`; it accepts Worker/account identity, resource identities, secret descriptors, gates, and durability only from `target`. Source component and full revision come from Homeboy, while the checkout path comes from `HOMEBOY_COMPONENT_PATH`. Fields crossing those ownership boundaries are rejected. The extension verifies `policy.reference.digest` as SHA-256 over Homeboy canonical JSON before using the policy.

Layered resource overlays require a repository-owned JSON or JSONC Wrangler template. D1 and R2 entries and queue producers match exactly one declared binding; queue consumers match the template's declared order. The private target may replace only database name/ID, bucket name, producer queue name, consumer queue name, dead-letter queue name, and account ID. The provider preserves source entry points, compatibility policy, schedules, binding names, queue tuning, and every other repository-owned field. It writes the merged config outside the checkout with mode `0600`, removes it after execution, and omits its path and private resource values from evidence. Legacy contracts continue to use their declared Wrangler config unchanged.

### Legacy Contract

Start from [`examples/deploy-contract.json`](examples/deploy-contract.json):

```sh
homeboy extension run cloudflare-workers -- --contract deploy-contract.json
```

The contract requires a clean worktree, exact Git revision, Wrangler config, Worker and account identifiers, expected binding names, secret descriptors, and HTTP gates. A repository-owned contract can set `repository.worktree` to `.` to resolve the Git root containing the contract and `repository.revision` to `HEAD` to resolve the checked-out full commit SHA before preflight. Result evidence always records that concrete SHA. Literal worktree paths and immutable SHAs remain available for externally generated contracts. Optional `predeploy_commands` run after immutable-source and provider preflight but before secret writes or Worker deployment. Each command declares a safe ID, direct executable, bounded string argument array, optional repository-contained working directory, and optional timeout. Shell executables and working-directory traversal are rejected. A command may reference one separately declared `secret_inputs` ID through `stdin_secret_input`; up to 64 KiB of exact private bytes are delivered only on stdin. Input environment variables are removed from every child environment, input files and resolved symlinks remain repository-contained, and secret inputs are never provisioned as Worker bindings. Evidence records only command IDs, status, and elapsed time; stdout, stderr, arguments, paths, input descriptors, and values are omitted. A pre-deploy failure or any resulting source drift stops before Worker mutation.

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

The `cli` manifest exposes the generic CLI-compatible surface. Current Homeboy also requires an `executable.runtime.run_command` to run an extension, so this extension supplies that runtime adapter for the same command. `deployment_providers` declares the generic apply and dry-run commands that Homeboy lowers through its normal deploy lifecycle.

Set `durability.redeploy_same_revision` to redeploy the same checked-out revision after the first successful gate sequence and run the same gates again. Secrets are provisioned once before the first deploy; set `durability.rotate_secrets: true` only when the durability proof explicitly requires a second secret write. This proves the declared external behavior survives a runtime replacement; it does not prove durability of state that the declared gates do not exercise.

## Limits

The first contract targets Wrangler 4.112's `deployments list --json` response and `rollback <version-id>` command. A weighted multi-version deployment is rejected because no version selection policy is in this initial contract. It intentionally does not infer bindings, routes, secret names, or gates from application code. Declare those externally so the same extension can deploy any Worker.
