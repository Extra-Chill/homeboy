# SSH Checkout Deploy

A Homeboy deployment provider for platforms that deploy from a long-lived remote checkout. You put code on a remote host's working tree and run that host's own deploy command, which is often guarded by a shared lock.

`homeboy deploy <project> <component>` runs these stages in order:

1. **source:** Homeboy's clean component `HEAD` is the exact revision to deploy.
2. **freshness:** that revision must be the tip of the configured branch on the freshness remote (`git ls-remote`). If it isn't, the deploy is refused, so it can never roll back newer commits.
3. **remote_preflight:** the remote checkout must have no local changes, because the host's deploy ships whatever is in that tree. If `running_probe` is set, no matching deploy process may be running.
4. **sync:** push the revision to `deploy_ref` on the remote checkout over SSH, check it out, and verify the remote `HEAD` and a clean tree.
5. **deploy:** run `deploy_command` in a login shell on the remote. If the output matches `busy_pattern`, the lock is held elsewhere: wait `lock_retry.delay_ms`, re-verify the remote checkout, and retry, up to `lock_retry.attempts`. When the output matches `success_pattern`, its named groups become deploy evidence. If the connection drops mid-deploy, it fails closed and is not retried, so a deploy that may still be running is never started twice.

`homeboy deploy --dry-run` runs stages 1–3 only. It never pushes or deploys.

## Configuration

Attach the provider in the Homeboy project, not in the deployed repository:

```json
{
  "components": [
    {
      "id": "app",
      "local_path": "/path/to/app",
      "deployment_provider": {
        "extension": "ssh-checkout-deploy",
        "provider": "ssh-checkout-deploy.deploy",
        "policy": {
          "branch": "main",
          "deploy_command": "deploy app",
          "busy_pattern": "Deploy lock held by (?<holder>\\S+)",
          "success_pattern": "Revision (?<revision>\\d+) successfully deployed",
          "running_probe": "run-deploy .* app",
          "lock_retry": { "attempts": 40, "delay_ms": 45000 },
          "timeout_ms": 1800000
        }
      },
      "deployment_provider_input": {
        "ssh_host": "build-host",
        "remote_path": "/srv/app",
        "freshness_remote": "https://git.example.com/org/app.git",
        "freshness_git_config": { "http.proxy": "socks5h://127.0.0.1:8080" }
      }
    }
  ]
}
```

| Policy (how this repository deploys) | |
|---|---|
| `branch` | The branch whose tip must equal the source revision. |
| `deploy_command` | The remote command, run as `bash -lc`, with shell aliases enabled. |
| `success_pattern` | Required. A JavaScript regex; its named groups are recorded as evidence. |
| `busy_pattern` | Optional. Output meaning the shared deploy lock is held; the named group `holder` is recorded. |
| `running_probe` | Optional. A `pgrep -f` pattern for an in-flight deploy on the remote. |
| `lock_retry` | Optional. `{ attempts, delay_ms }`; defaults to one attempt. |
| `deploy_ref` | Optional. The remote ref the revision is pushed to; defaults to `refs/heads/homeboy-deploy`. |
| `timeout_ms` | Optional. The push and deploy timeout; defaults to 30 minutes. |

| Target (which environment) | |
|---|---|
| `ssh_host` | An SSH host alias. `ssh -o BatchMode=yes` must reach it non-interactively. |
| `remote_path` | The absolute path of the remote checkout. |
| `freshness_remote` | The Git remote or URL whose `branch` tip gates the deploy. |
| `freshness_git_config` | Optional `git -c` settings for that lookup, such as a proxy. |

Because provider deploys use the component checkout's clean `HEAD`, fast-forward that checkout to the branch tip before deploying. The freshness stage enforces this.

## Result

The provider prints `homeboy/ssh-checkout-deploy-result/v1` with:
- `source`
- `target`
- per-stage status and timing
- `lock_waits` (attempt, holder and wait for each)
- `deploy` (the success pattern's named groups)
- `failure` and `remediation` when a stage fails

Remote checkout failures include the first 20 `git status --porcelain` path entries in `failure.paths` and in `failure.message`; if more paths are present, the message ends with `(and N more)`. The `remote_checkout_dirty` remediation identifies the remote path and host to inspect. Evidence contains paths only, never file contents.

## Tests

```sh
node ssh-checkout-deploy/tests/ssh-checkout-deploy.test.mjs
```

The tests use real Git repositories and a fake `ssh` that runs remote commands locally. They cover the dry run, lock retry, lock exhaustion, deploy failure, a dropped connection, a dirty remote, a running deploy, a stale source, a tampered policy, and a moved source.
