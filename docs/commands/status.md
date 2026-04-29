# `homeboy status`

Show an actionable component status overview.

## Synopsis

```sh
homeboy status [PROJECT]
```

## Common filters

- `--full` — show the full workspace/context report
- `--uncommitted` — show only components with uncommitted changes
- `--needs-bump` — show only components that need a version bump
- `--ready` — show only components ready to deploy
- `--docs-only` — show only components with docs-only changes
- `--all` — show all components regardless of current directory context
- `--outdated` — show only outdated components

## Related

- [component](component.md)
- [project](project.md)
- [triage](triage.md)
