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

Supported fidelity values are `local-equivalent`, `local-partial`, `remote-only`, and `unknown`.

## Output

The JSON output uses command `ci.list` and includes an `inventory` object with `profiles`, `jobs`, and `discovered_jobs`.

`ci run` uses command `ci.run` and includes the selected job/profile, per-job command output, per-job exit codes, and an aggregate `success` / `exit_code`.

Refs: [#1977](https://github.com/Extra-Chill/homeboy/issues/1977)
