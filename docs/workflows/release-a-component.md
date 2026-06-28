# Release A Component

Homeboy release workflows turn conventional commits and component metadata into version bumps, changelogs, tags, artifacts, and optional publish/deploy steps.

## Inspect First

```bash
homeboy changes
homeboy version show
homeboy changelog show
homeboy release --dry-run
```

## Apply Deliberately

Release commands are operator actions. Use dry-run output first, then apply only when the plan is correct:

```bash
homeboy release --apply
```

## Code Factory Context

The broader Code Factory model is lint + fix, test + fix, audit + fix, release, and deploy. See [Code Factory](../concepts/code-factory.md).

## Reference

- [release command](../commands/release.md)
- [version command](../commands/version.md)
- [changes command](../commands/changes.md)
- [changelog command](../commands/changelog.md)
