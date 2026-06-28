# Release A Component

Homeboy release workflows turn conventional commits and component metadata into version bumps, changelogs, tags, artifacts, and optional publish/deploy steps.

## Use This When

- A component has releasable commits and configured version targets.
- You need a dry-run release plan before mutating tags, versions, or artifacts.
- A package needs to be regenerated for an existing tag.
- An already-tagged release needs to be finished from artifacts.

## 1. Inspect The Release State

Start with read-only commands:

```bash
homeboy changes <component-id>
homeboy version show <component-id>
homeboy changelog show <component-id>
```

These commands tell you whether the commit history, configured version targets, and changelog state line up before the release planner mutates anything.

## 2. Dry-Run The Plan

Always inspect the plan before applying it:

```bash
homeboy release <component-id> --dry-run
```

For automation, capture the plan:

```bash
homeboy --output homeboy-results/release-plan.json \
  release <component-id> --dry-run
```

Check the planned version bump, files to update, tags to create, publish steps, and skipped-release reasons.

## 3. Run The Quality Gate

Before applying a release, prove the branch with the normal review gate:

```bash
homeboy review <component-id> --changed-since origin/main
```

Use runner routing when release proof must be non-local. See [Use runners](use-runners.md) and [Release-gate proof path](../operations/release-gate-proof-path.md).

## 4. Apply Deliberately

Release commands are operator actions. Apply only after the dry-run and quality gate are acceptable:

```bash
homeboy release <component-id> --apply
```

Expected mutations can include version target edits, changelog finalization, commits, tags, pushes, and release artifact publishing depending on component configuration.

## 5. Recovery Paths

Regenerate a package for an existing tag:

```bash
homeboy release <component-id> --package-only --tag v1.2.3 --apply
```

Finish an already-tagged release from artifacts:

```bash
homeboy release <component-id> --head --from-artifacts ./artifacts --skip-checks --apply
```

Use these intentionally. They are recovery/operator paths, not the default release flow.

## Code Factory Context

The broader Code Factory model is lint + fix, test + fix, audit + fix, release, and deploy. See [Code Factory](../concepts/code-factory.md).

## Reference

- [release command](../commands/release.md)
- [version command](../commands/version.md)
- [changes command](../commands/changes.md)
- [changelog command](../commands/changelog.md)
- [review workflow](review-a-branch.md)
