# git

Git helper commands for components.

```bash
homeboy git status <component-id>
homeboy git commit <component-id> <message>
homeboy git push <component-id> [--tags]
homeboy git pull <component-id>
homeboy git tag <component-id> <tag-name> [--message <message>]
```

## Subcommands

### status

Show git status for a component.

```bash
homeboy git status <component-id>
```

### commit

Stage all changes and commit.

```bash
homeboy git commit <component-id> "Commit message"
```

### push

Push local commits to remote.

```bash
homeboy git push <component-id>
homeboy git push <component-id> --tags
```

### pull

Pull remote changes.

```bash
homeboy git pull <component-id>
```

### tag

Create a git tag for a component.

```bash
homeboy git tag <component-id> v1.0.0
homeboy git tag <component-id> v1.0.0 --message "Release v1.0.0"
```
