# project

Manage project configuration. Configuration is shared between Homeboy CLI and Homeboy Desktop.

## What a project is

A project represents a remote (and optionally local) environment with:
- a project type (e.g. `wordpress`, `nodejs`)
- optional server linkage (SSH host/user/port)
- optional database settings
- optional components for deployment

## Common usage

```bash
homeboy projects
homeboy project show [id]
homeboy project switch <id>
```

For the complete list of `project` subcommands and flags:

```bash
homeboy docs "project subcommands"
```
