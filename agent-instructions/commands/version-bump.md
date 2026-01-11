---
name: version-bump
description: Bump component version and update changelog via Homeboy.
version: 0.1.0
allowed-tools: Bash(homeboy *)
---

# Version bump

Use Homeboy to update the version and changelog together. Do not manually edit changelog files.

## Workflow

1. `homeboy component show <componentId>`
2. `homeboy version show <componentId>`
3. Decide bump interval: `patch|minor|major`
4. Bump version and add changelog entries (repeat `--changelog-add` per item):

```sh
homeboy version bump <componentId> <patch|minor|major> \
  --changelog-add "<change 1>" \
  --changelog-add "<change 2>" \
  --changelog-finalize
```

- Omit `--changelog-finalize` if you are intentionally keeping items under "Unreleased".
- Use `--changelog-empty-ok` only when an empty changelog is explicitly acceptable.

5. `homeboy build <componentId>`
6. `homeboy git commit <componentId> "Bump version to X.Y.Z"`
7. `homeboy git push <componentId>`

## Notes

- Tagging is a separate release concern. Only use `homeboy git tag` and `homeboy git push <componentId> --tags` when explicitly doing a release.
