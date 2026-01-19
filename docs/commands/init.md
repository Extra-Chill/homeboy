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
    "next_steps": [
      "Read CLAUDE.md and README.md for repo-specific guidance.",
      "Run `homeboy docs documentation/index` for Homeboy documentation.",
      "Run `homeboy docs commands/commands-index` to browse available commands."
    ],
    "servers": [
      { "id": "server-id", "host": "...", "user": "...", "port": 22 }
    ],
    "projects": [
      { "id": "project-id", "domain": "example.com" }
    ],
    "components": [
      {
        "id": "component-id",
        "local_path": "...",
        "remote_path": "...",
        "build_artifact": "...",
        "build_command": "./build.sh",
        "version_targets": [{ "file": "plugin.php", "pattern": "..." }]
      }
    ],
    "modules": [
      {
        "id": "module-id",
        "name": "...",
        "version": "...",
        "ready": true,
        "ready_reason": null,
        "ready_detail": null,
        "compatible": true
      }
    ],
    "version": {
      "component_id": "component-id",
      "version": "0.20.7",
      "targets": [{ "file": "Cargo.toml", "pattern": "...", "full_path": "...", "match_count": 1 }]
    },
    "git": {
      "branch": "main",
      "clean": true,
      "ahead": 0,
      "behind": 0,
      "commits_since_version": 5,
      "version_baseline": "v0.20.7"
    },
    "last_release": {
      "tag": "v0.20.7",
      "date": "2026-01-15",
      "summary": "Add -m flag for changelog add command"
    },
    "changelog": {
      "path": "docs/changelog.md",
      "label": "Unreleased",
      "items": ["Queued change"]
    },
    "agent_context_files": [
      "CLAUDE.md",
      "README.md",
      "docs/index.md"
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
| `next_steps` | Actionable guidance for agents and onboarding |
| `servers`, `projects`, `components` | Available resources for reference |
| `modules` | Available Homeboy modules |
| `version` | Current component version snapshot (first matched component) |
| `git` | Branch + clean state + ahead/behind snapshot + release state |
| `git.commits_since_version` | Number of commits since last version tag (indicates unreleased work) |
| `git.version_baseline` | Tag or commit hash used as baseline for commit count |
| `last_release` | Latest changelog release summary |
| `changelog` | Unreleased preview from component changelog |
| `agent_context_files` | Git-tracked markdown files for AI agent context (CLAUDE.md, README.md, etc.) |

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
homeboy project create "<name>" <domain> --server <server_id> --module <module_id>
```

**Component** (buildable/deployable unit):
```bash
homeboy component create "<name>" --local-path "." --remote-path "<path>" --project <project_id>
homeboy component set <id> --build-command "./build.sh" --build-artifact "build/<name>.zip"
```

## Derivation Rules

1. **name**: Directory name or from workspace docs
2. **remotePath**: Match existing component patterns in target project
3. **buildArtifact/buildCommand**: From build.sh, Makefile, or workspace docs
4. **domain**: ASK (cannot derive locally)
5. **server_id**: Auto-select if only one exists

## Verification

```bash
homeboy context  # Confirm managed: true
```
