# Cloudflare Workers Extension

`cloudflare-workers` is Homeboy Extensions' provider-owned deployment command for a generic Cloudflare Worker. It takes one immutable contract file, verifies the clean checkout and Wrangler target, applies declared secrets through stdin, deploys, runs HTTP gates, and rolls back to the deployment recorded before the attempt when secret provisioning, deployment, or a gate fails.

## Contract

Start from [`examples/deploy-contract.json`](examples/deploy-contract.json):

```sh
homeboy extension run cloudflare-workers -- --contract deploy-contract.json
```

The contract requires a clean worktree, exact Git revision, Wrangler config, Worker and account identifiers, expected binding names, secret descriptors, and HTTP gates. A secret has exactly one descriptor: an environment variable name or a file path. Values are read only at execution time, written to a temporary `0600` file, supplied to `wrangler secret put` through stdin, and removed in a `finally` cleanup path. They are never command arguments, evidence fields, or raw provider output.

The result is `homeboy/cloudflare-worker-deploy-result/v1`. It records source revision, redacted deterministic stage progress, prior and deployed Worker deployment/version IDs, rollback status, and remediation. Raw Wrangler output and gate bodies are omitted.

The `cli` manifest exposes the generic CLI-compatible surface. Current Homeboy also requires an `executable.runtime.run_command` to run an extension, so this extension supplies that runtime adapter for the same command. The `deployment_providers` descriptor is retained as declarative metadata for a future Homeboy deployment-provider lowering; it is not invoked by current Homeboy releases.

Set `durability.redeploy_same_revision` to redeploy the same checked-out revision after the first successful gate sequence and run the same gates again. Secrets are provisioned once before the first deploy; set `durability.rotate_secrets: true` only when the durability proof explicitly requires a second secret write. This proves the declared external behavior survives a runtime replacement; it does not prove durability of state that the declared gates do not exercise.

## Limits

The first contract targets Wrangler 4.112's `deployments list --json` response and `rollback <version-id>` command. A weighted multi-version deployment is rejected because no version selection policy is in this initial contract. It intentionally does not infer bindings, routes, secret names, or gates from application code. Declare those externally so the same extension can deploy any Worker.
