# Agent Hooks

Unified hooks for Claude Code and OpenCode that enforce Homeboy usage patterns across all projects.

## Installation

```bash
homeboy module install agent-hooks
```

The setup script automatically installs hooks for both AI coding assistants:
- **Claude Code**: Hooks in `~/.claude/hooks/agent-hooks/`, configuration in `~/.claude/settings.json`
- **OpenCode**: Plugin at `~/.config/opencode/plugins/homeboy-plugin.ts`

## Supported Agents

| Feature | Claude Code | OpenCode |
|---------|-------------|----------|
| Session start message | SessionStart hook | Plugin init |
| Bash anti-pattern detection | PreToolUse (Bash) | tool.execute.before |
| File protection | PreToolUse (Edit) | tool.execute.before |

## Hooks

### Session Start: Init Reminder

When starting any session, displays a reminder:

```
Homeboy Active

Start with: homeboy init
This gathers context (components, servers, versions) before operations.

Use Homeboy for: builds, deploys, version management
Docs: homeboy docs commands/commands-index
```

### Bash Anti-Pattern Detector

Blocks bash commands that bypass Homeboy:

| Pattern | Homeboy Alternative |
|---------|---------------------|
| `git status` | `homeboy changes` |
| `./build.sh` | `homeboy build <component>` |
| `rsync ... user@host:...` | `homeboy deploy` |
| `scp ... user@host:...` | `homeboy deploy` |
| `npm version` | `homeboy version bump/set` |
| `cargo set-version` | `homeboy version bump/set` |

### Dynamic File Protection

Uses `homeboy init --json` to dynamically detect protected files:

- **Version targets**: Files listed in `version.targets[].full_path`
- **Changelog**: File at `changelog.path`

This approach:
- Works for ANY project type (Rust, Node, WordPress, Swift, PHP)
- Stays in sync with actual Homeboy configuration
- No hardcoded patterns to maintain

## Behavior

Hooks apply globally to all sessions. In non-Homeboy repositories, hooks gracefully pass through (homeboy init returns empty data).

## Uninstall

```bash
homeboy module run agent-hooks uninstall
```

Or manually:
1. Claude Code: Remove `~/.claude/hooks/agent-hooks/` and clean `~/.claude/settings.json`
2. OpenCode: Remove `~/.config/opencode/plugins/homeboy-plugin.ts`

## Structure

```
agent-hooks/
├── agent-hooks.json      # Module manifest
├── setup.sh              # Unified installer (both agents)
├── uninstall.sh          # Unified uninstaller (both agents)
├── README.md
├── core/                 # Shared logic (bash)
│   └── patterns.sh       # Bash anti-pattern detection
├── claude/               # Claude Code hooks (bash)
│   ├── session-start.sh
│   ├── pre-tool-bash.sh
│   └── pre-tool-edit.sh
└── opencode/             # OpenCode plugin (TypeScript)
    └── homeboy-plugin.ts
```

## Architecture Notes

**Claude Code** uses separate bash scripts for each hook type, configured via `~/.claude/settings.json`.

**OpenCode** uses a single TypeScript plugin that exports multiple hook handlers, installed to `~/.config/opencode/plugins/`.

Both implementations provide identical functionality and error messages for seamless switching between agents.
