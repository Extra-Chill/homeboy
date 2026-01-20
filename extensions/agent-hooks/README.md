# Agent Hooks

Claude Code hooks that enforce Homeboy usage patterns across all projects.

## Installation

```bash
homeboy module install agent-hooks
```

The setup script automatically:
1. Copies hooks to `~/.claude/hooks/agent-hooks/`
2. Merges configuration into `~/.claude/settings.json`

## Hooks

### SessionStart: Init Reminder

When starting any Claude Code session, displays a reminder:

```
Homeboy Active

Start with: homeboy init
This gathers context (components, servers, versions) before operations.

Use Homeboy for: builds, deploys, version management
Docs: homeboy docs commands/commands-index
```

### PreToolUse (Bash): Anti-Pattern Detector

Blocks bash commands that bypass Homeboy:

| Pattern | Homeboy Alternative |
|---------|---------------------|
| `git status` | `homeboy changes` |
| `./build.sh` | `homeboy build <component>` |
| `rsync ... user@host:...` | `homeboy deploy` |
| `scp ... user@host:...` | `homeboy deploy` |
| `npm version` | `homeboy version bump/set` |
| `cargo set-version` | `homeboy version bump/set` |

### PreToolUse (Edit): Dynamic File Protection

Uses `homeboy init --json` to dynamically detect protected files:

- **Version targets**: Files listed in `version.targets[].full_path` (Cargo.toml, package.json, Info.plist, VERSION, etc.)
- **Changelog**: File at `changelog.path` (typically CHANGELOG.md or docs/changelog.md)

This approach:
- Works for ANY project type (Rust, Node, WordPress, Swift, PHP)
- Stays in sync with actual Homeboy configuration
- No hardcoded patterns to maintain

## Behavior

Hooks apply globally to all Claude Code sessions. In non-Homeboy repositories, the edit hook gracefully passes through (homeboy init returns empty data).

## Uninstall

```bash
bash ~/.claude/hooks/agent-hooks/../uninstall.sh
```

Or manually:
1. Remove `~/.claude/hooks/agent-hooks/`
2. Remove agent-hooks entries from `~/.claude/settings.json`

## Structure

```
agent-hooks/
├── agent-hooks.json      # Module manifest
├── setup.sh              # Installation script
├── uninstall.sh          # Removal script
├── core/                 # Shared logic (hub)
│   └── patterns.sh       # Bash anti-pattern detection
├── claude/               # Claude Code hooks (spoke)
│   ├── session-start.sh
│   ├── pre-tool-bash.sh
│   └── pre-tool-edit.sh
└── README.md
```

## Future Expansion

This module is designed for future agent support:
- `cursor/` - Cursor AI rules
- `copilot/` - GitHub Copilot instructions

The `core/` hub contains shared detection logic reusable across agents.
