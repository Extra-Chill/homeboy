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
3. Decide bump interval based on code-level changes and git logs since previous version bump: `patch|minor|major`
4. Add changelog entries (preferred: JSON spec):

```sh
homeboy changelog add --json '{"componentId":"<componentId>","messages":["<change 1>","<change 2>"]}'
```

5. Bump version and finalize changelog:

```sh
homeboy version bump <componentId> <patch|minor|major>
```

6. `homeboy build <componentId>`
7. `homeboy git commit <componentId> "Bump version to X.Y.Z"`
8. `homeboy git push <componentId>`

## Notes

- Ask the user if you should also use `homeboy git tag` and `homeboy git push <componentId> --tags` 
