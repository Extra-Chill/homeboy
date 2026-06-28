# Reproduce CI

Use Homeboy CI workflows when a component declares CI profiles that can be run through Homeboy's command surface. This is different from parsing arbitrary provider YAML.

## Use This When

- You need to list the CI profiles a component exposes to Homeboy.
- A failing PR check maps to a declared Homeboy lint, test, bench, or review profile.
- You need differential classification between a baseline command and a head command.
- An autofix job needs a dry-run transaction before pushing changes.

## 1. List Declared Profiles

Start by asking Homeboy what it knows about the component's CI surface:

```bash
homeboy ci list --path /path/to/repo --extension <extension-id>
```

Use discovered provider inventory as context, not as proof that every provider job is locally reproducible.

## 2. Run Command-Native Jobs

When a declared job maps to a Homeboy command, run it through that command's normal workflow:

```bash
homeboy ci run --path /path/to/repo --extension <extension-id> --job <job-id>
homeboy ci run --path /path/to/repo --extension <extension-id> --profile <profile-id>
```

For jobs that map directly to native Homeboy commands, you can also run the command itself:

```bash
homeboy lint <component-id> --ci-job <job-id>
homeboy test <component-id> --ci-job <job-id>
homeboy bench <component-id> --ci-profile <profile-id>
```

For a PR-shaped gate with an additional declared CI profile:

```bash
homeboy review <component-id> --changed-since origin/main --ci-profile pr
```

## 3. Capture The CI Context

Use `--output` when a CI reproduction is intended for automation or agents:

```bash
homeboy --output homeboy-results/ci-test.json \
  test <component-id> --ci-job <job-id>
```

Command-native CI reproduction includes `ci_context` metadata so downstream tools can see which declared job/profile was selected.

## 4. Classify Baseline Versus Head

Use differential classification when you need to distinguish pre-existing failures from branch-introduced failures:

```bash
homeboy ci differential-gate \
  --baseline-command 'homeboy test <component-id> --changed-since origin/main' \
  --baseline-exit-code 0 \
  --head-command 'homeboy test <component-id> --changed-since origin/main' \
  --head-exit-code 1
```

This is useful for reporting whether a branch newly broke the gate or inherited an existing failure.

## 5. Dry-Run Autofix Transactions

When CI is allowed to push autofixes, dry-run the transaction first:

```bash
homeboy ci autofix \
  --path /path/to/repo \
  --target-repo owner/repo \
  --target-branch pr-head-branch \
  --message "chore(ci): apply Homeboy autofixes" \
  --dry-run
```

The dry-run should report the changed files, target route, and whether it would push directly or create an autofix branch.

## Reference

- [ci command](../commands/ci.md)
- [review workflow](review-a-branch.md)
- [capture evidence](capture-evidence.md)
