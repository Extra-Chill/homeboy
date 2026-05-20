# `homeboy ci`

Inspect CI reproduction profiles and shallow CI discovery for a checkout.

## List Profiles

```sh
homeboy ci list --path /path/to/repo --extension nodejs
```

`ci list` resolves the same path-only component context as other Homeboy commands, then prints:

- declared extension-owned CI profiles
- declared extension-owned CI jobs
- best-effort discovered CI surfaces such as `.github/workflows/*.yml` and `.buildkite/*.yml`

Discovery is intentionally inventory-only. Homeboy does not infer runnable local equivalence from arbitrary CI YAML; runnable jobs come from explicit extension profile declarations.

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

Refs: [#1977](https://github.com/Extra-Chill/homeboy/issues/1977)
