# `homeboy init`

Initialize a repo for use with Homeboy. Returns comprehensive context data in JSON format.

## Usage

```bash
homeboy init
```

## Getting Started

Run `homeboy init` to gather all context in one call:
- Current directory state (managed, components, gaps)
- Available servers, projects, components, modules

Then read workspace docs (CLAUDE.md, README.md) for project context.

## Output Structure

```json
{
  "success": true,
  "data": {
    "command": "init",
    "context": {
      "cwd": "/path/to/repo",
      "git_root": "/path/to/repo",
      "managed": true,
      "matched_components": ["component-id"],
      "contained_components": [],
      "project": { "id": "project-id", "domain": "example.com" },
      "components": [{ "id": "...", "build_artifact": "...", "gaps": [...] }],
      "suggestion": "Run homeboy deploy..."
    },
    "servers": [
      { "id": "server-id", "host": "...", "user": "...", "port": 22 }
    ],
    "projects": [
      { "id": "project-id", "domain": "example.com" }
    ],
    "components": [
      { "id": "component-id", "local_path": "...", "remote_path": "...", "build_artifact": "..." }
    ],
    "modules": [
      { "id": "module-id", "name": "...", "version": "...", "ready": true, "compatible": true }
    ]
  }
}
```

## Output Interpretation

| Field | Meaning |
|-------|---------|
| `context.managed` | true = repo has registered component(s) |
| `context.matched_components` | Components matching current path |
| `context.contained_components` | Components in subdirectories (monorepo) |
| `context.components[].gaps` | Missing config with remediation commands |
| `servers`, `projects`, `components` | Available resources for reference |
| `modules` | Available Homeboy modules |

## Decision Tree

### If `managed: true`
Repo is configured. Check for gaps and complete setup.

```bash
# Gaps include remediation commands - run them
homeboy component set <id> --build-command "./build.sh"
homeboy component set <id> --changelog-targets '["CHANGELOG.md"]'
```

### If `managed: false` with `containedComponents`
Monorepo root - components exist in subdirectories. Check gaps, skip creation.

### If `managed: false` (empty)
Create based on workspace docs:

**Project** (deployable environment with domain):
```bash
homeboy project create "<name>" <domain> --server <serverId> --module <moduleId>
```

**Component** (buildable/deployable unit):
```bash
homeboy component create "<name>" --local-path "." --remote-path "<path>" --project <projectId>
homeboy component set <id> --build-command "./build.sh" --build-artifact "build/<name>.zip"
```

## Derivation Rules

1. **name**: Directory name or from workspace docs
2. **remotePath**: Match existing component patterns in target project
3. **buildArtifact/buildCommand**: From build.sh, Makefile, or workspace docs
4. **domain**: ASK (cannot derive locally)
5. **serverId**: Auto-select if only one exists

## Verification

```bash
homeboy context  # Confirm managed: true
```
