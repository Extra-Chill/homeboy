# `homeboy ci`

Inspect and run explicit CI reproduction profiles for a checkout.

## List Profiles

```sh
homeboy ci list --path /path/to/repo --extension nodejs
```

`ci list` resolves the same path-only component context as other Homeboy commands, then prints:

- declared extension-owned CI profiles
- declared extension-owned CI jobs
- best-effort discovered CI surfaces such as `.github/workflows/*.yml` and `.buildkite/*.yml`

Discovery is intentionally inventory-only. Homeboy does not infer runnable local equivalence from arbitrary CI YAML; runnable jobs come from explicit extension profile declarations.

## Run Declared Jobs

Run one extension-declared job:

```sh
homeboy ci run --path /path/to/repo --extension nodejs --job lint-typecheck
```

Run every job in an extension-declared profile:

```sh
homeboy ci run --path /path/to/repo --extension nodejs --profile pr
```

`ci run` executes only jobs declared under the extension manifest's `ci.jobs` contract. It does not parse arbitrary provider YAML into runnable local commands. Commands run from the component root with the declared `args` and `env`; the command's exit code is captured in JSON and the overall command exits non-zero when any selected job fails.

For command-native reproduction, `homeboy lint --ci-job <ID>` and `homeboy test --ci-job <ID>` select a single declared job whose `command` is `lint` or `test` respectively, then run through the normal Homeboy lint/test workflow with the job's declared local context applied. `homeboy bench --ci-profile <ID>` selects a single-job profile whose job declares `command: "bench"` and runs it through the normal bench workflow.

## Autofix Transaction

```sh
homeboy ci autofix --path /path/to/repo \
  --target-repo owner/repo --target-branch pr-head-branch \
  --message "chore(ci): homeboy autofix — refactor (3 files)"
```

`ci autofix` owns the end-to-end CI autofix transaction so the GitHub Action is a thin caller instead of re-implementing branch/commit/push orchestration in shell. It assumes the working tree already contains the autofix changes to commit, then:

- stages all working-tree changes and skips cleanly when nothing is staged,
- classifies the staged paths against the component's computed `drift_files` (see `homeboy component show`),
- routes drift-only changes as a direct push (distinct commit prefix that does not count toward the autofix cap) and authored fixes as an autofix-branch push,
- commits with the CI bot identity (single source of truth shared with the autofix guards), and
- resolves the push target (`origin` for the same repo without a token, an authenticated `x-access-token` URL when `--token`/`APP_TOKEN` is set, an anonymous URL for cross-repo pushes) and pushes `HEAD` to `--target-branch`.

`--dry-run` classifies the changes and resolves the push target without committing or pushing. The JSON output uses command `ci.autofix` and includes `push_target`, the `route` (`direct-drift` or `autofix-branch`), `changed_files`, `committed`, and a machine-readable `status` (`pushed`, `no-changes`, `push-failed`, or `dry-run`).

## Manifest Shape

Extensions can declare CI profiles with a `ci` block:

```json
{
  "ci": {
    "profiles": {
      "pr": {
        "label": "Pull request checks",
        "jobs": ["lint-typecheck", "unit"]
      }
    },
    "jobs": {
      "lint-typecheck": {
        "check_names": ["Lint and typecheck"],
        "command": "lint",
        "env": { "CI": "1" },
        "fidelity": "local-equivalent"
      }
    }
  }
}
```

For `homeboy test --ci-job` and `homeboy bench --ci-profile`, job `args` are forwarded as runner passthrough arguments before any explicit CLI passthrough arguments. For `homeboy lint --ci-job`, job `env` is applied to the lint runner; use extension settings or env-backed runner behavior for lint-specific variants.

Supported fidelity values are `local-equivalent`, `local-partial`, `remote-only`, and `unknown`.

## Output

The JSON output uses command `ci.list` and includes an `inventory` object with `profiles`, `jobs`, and `discovered_jobs`.

`ci run` uses command `ci.run` and includes the selected job/profile, per-job command output, per-job `ci_context`, per-job exit codes, and an aggregate `success` / `exit_code`.

Command-native CI reproduction also includes `ci_context` when a CI selector is used. `homeboy lint --ci-job <ID>`, `homeboy test --ci-job <ID>`, and `homeboy bench --ci-profile <ID>` preserve the normal command output shape and add the selected job's mapping metadata:

```json
{
  "ci_context": {
    "profile": "pr",
    "job_id": "unit",
    "check_names": ["test-unit-jspi"],
    "provider": "github-actions",
    "workflow": "ci.yml",
    "fidelity": "local-partial",
    "limitations": ["CI matrix shards are not reproduced locally"]
  }
}
```

Refs: [#1977](https://github.com/Extra-Chill/homeboy/issues/1977)
