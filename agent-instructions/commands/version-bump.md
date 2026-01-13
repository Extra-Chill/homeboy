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
3. Review changes since last version tag:

```sh
homeboy changes <componentId>
```

4. Based on the changes output, decide bump interval: `patch|minor|major`
5. Add changelog entries:

```sh
homeboy changelog add --json '{"componentId":"<componentId>","messages":["<change 1>","<change 2>"]}'
# (Alternative non-JSON mode)
# homeboy changelog add <componentId> "<change 1>"
```

6. Bump version and finalize changelog:

```sh
homeboy version bump <componentId> <patch|minor|major>
```

7. `homeboy build <componentId>`
8. `homeboy git commit <componentId> "Bump version to X.Y.Z"`
9. `homeboy git push <componentId>`

## Notes

- Ask the user if you should also use `homeboy git tag` and `homeboy git push <componentId> --tags` 
