# Release-gate proof: the canonical non-local command path

This is the one documented command path agents should use to produce
**release-gate proof** from a worktree. It uses normal Homeboy routing, preserves
Lab/offload policy, and never requires local-hot bypass flags.

The problem it solves: when the normally installed Homeboy gate fails early (stale
runner binary, missing secrets, a resource-policy refusal), the fallback is not
obvious, and the fastest-looking option — a local `cargo`/source invocation or a
`--force-hot` rerun — is **not** acceptable proof. This page makes the boundary
explicit.

## TL;DR

- **Canonical proof command:** `homeboy review [component] --changed-since=<base>`
  — the audit → lint → test umbrella, run through normal routing.
- **Routing:** let automatic Lab routing handle it, or pass `--runner <id>`
  explicitly. Never run the gate locally on a warm/hot machine.
- **`--force-hot` / `--allow-local-hot` are debugging aids, not proof.** A gate
  result produced with either flag is not release-gate evidence.
- **If the installed Homeboy is stale or missing secrets, repair the routing**
  (refresh the runner binary, sync the workspace, fix secrets) — do **not** fall
  back to a local-hot or source run.

## The canonical proof command

```bash
homeboy review --changed-since=<base-ref>
```

`homeboy review` is the thin umbrella that fans out scoped `audit`, `lint`, and
`test` against the same changed-file set and prints one consolidated report — the
canonical verification order a reviewer would see for your diff. See
[review](../commands/review.md) for scope flags, output shapes, and exit codes.

From a feature worktree, scope to the branch base so the gate only judges your
change:

```bash
# What a reviewer sees for just this branch's diff
homeboy review --changed-since=origin/main

# Pin a component and write the envelope to a file for evidence capture
homeboy --output "$RUNNER_TEMP/homeboy-results/review.json" \
  review my-component --changed-since=origin/main --summary
```

Run individual stages (`homeboy audit`, `homeboy lint`, `homeboy test`) only for a
deep dive into one stage. The umbrella `review` run is the proof artifact.

## Routing: keep it non-local

The gate stages are resource-pressure ("hot") commands. On a warm or hot machine,
Homeboy's resource policy intentionally refuses to start them locally from a
non-interactive shell and points you at Lab offload. Honor that routing:

- **Preferred — automatic Lab routing.** When a default Homeboy Lab runner is
  connected (or the component declares one), portable gate commands route to Lab
  offload automatically. Just run `homeboy review --changed-since=<base>` and let
  Homeboy pick the runner.
- **Explicit runner.** Pass `--runner <id>` to target a specific Lab runner when
  there is no inferable default, e.g. via the durable agent-task path:

  ```bash
  homeboy agent-task run-plan --runner homeboy-lab ...
  ```

- **Runner-side checkout.** When the intended checkout already lives on a Lab
  runner, dispatch the gate from that runner-side checkout through `runner exec`
  instead of forcing a controller-local run. `runner exec` marks the job as
  runner-hosted, so the gate passes the non-interactive resource preflight
  **without** `--force-hot`:

  ```bash
  homeboy runner exec homeboy-lab \
    --cwd /srv/homeboy/checkouts/my-component \
    -- homeboy review my-component --changed-since=origin/main
  ```

See [runner](../commands/runner.md) for runner selection, readiness, and managed
follow-up commands, and [the reverse-runner operator guide](controller-runner-reverse-runner.md)
for setup and smoke evidence.

## Local-hot bypass is a debugging aid, not proof

Homeboy exposes two local-hot bypass flags:

- `--force-hot` — suppress the resource-policy warning and run the command
  locally anyway.
- `--allow-local-hot` — allow a local-hot rerun of a command that has no portable
  Lab offload contract.

These exist so a human can debug source changes quickly on their own machine.
**They are not acceptable as release-gate proof:**

- A gate run produced with `--force-hot`/`--allow-local-hot` records
  `resource_policy.force_hot: true` in its observation metadata — it is explicitly
  marked as a bypassed local run, not a routed gate result.
- Running the gate from a source build (`cargo run -- ...`) or any non-installed
  invocation tests the source under your hands, not the routed binary the gate
  contract expects. Use it for iteration; do not present it as proof.

If you only have a local-hot result, you have a debugging signal, not a gate.
Re-run through normal routing before claiming the gate is green.

## When the installed Homeboy is stale or missing secrets

Early gate failures usually mean the routing needs repair, not that you should run
locally. Fix the routing, then re-run the canonical `homeboy review` command:

- **Stale runner binary or daemon.** Check `runner_homeboy` in `homeboy runner status <runner>`
  / offload metadata. If the daemon identity is stale, restart it; if the
  configured executable is behind merged fixes, upgrade it first:

  ```bash
  homeboy runner disconnect <runner> && homeboy runner connect <runner>
  homeboy upgrade --force --upgrade-runner <runner>
  ```

- **Stale runner workspace.** Materialize the current checkout into the Lab
  workspace before the gate run:

  ```bash
  homeboy runner workspace sync <runner> --path <path> --mode snapshot
  ```

- **Missing secrets.** A gate that fails with `secret_env_missing` /
  `failure_classification: "invalid_input"` needs the runner's secret env
  configured, not a local rerun. Inspect readiness with `homeboy runner env <runner>`
  and `homeboy runner doctor <runner>`, then configure the required secrets.

After the routing is healthy, re-run `homeboy review --changed-since=<base>` and
use that routed result as the proof.

## Rules of thumb

- Produce proof with `homeboy review --changed-since=<base>` through normal/Lab
  routing.
- Never use `--force-hot` or `--allow-local-hot` to manufacture a gate result.
- Never present a `cargo run`/source invocation as release-gate proof.
- Repair stale runners and missing secrets; do not fall back to local execution.

## Related

- [review](../commands/review.md) — the audit + lint + test umbrella (canonical proof command)
- [runner](../commands/runner.md) — Lab runner selection, offload routing, and managed follow-ups
- [Controller to runner reverse-runner setup](controller-runner-reverse-runner.md) — gated reverse-runner path
- [agent-task](../commands/agent-task.md) — durable lifecycle and runner-side gate dispatch
- [Code Factory](../code-factory.md) — the lint → test → audit → release pipeline this proof feeds
