# Agent Hooks

Unified hooks for Claude Code and OpenCode that enforce Homeboy usage patterns across all projects.

## Installation

```bash
homeboy module install agent-hooks
```

The setup script automatically installs hooks for both AI coding assistants:
- **Shared config**: `~/.config/homeboy/agent-message.txt` (centralized session message)
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

Use Homeboy for: builds, deploys, version management, documentation
Commands: homeboy docs commands/commands-index
Documentation: homeboy docs documentation/index
```

The message is centralized in `core/session-message.txt` and installed to `~/.config/homeboy/agent-message.txt` during setup.

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
3. Shared: Remove `~/.config/homeboy/agent-message.txt`

## Structure

```
agent-hooks/
├── agent-hooks.json      # Module manifest
├── setup.sh              # Unified installer (both agents)
├── uninstall.sh          # Unified uninstaller (both agents)
├── README.md
├── core/                 # Shared logic and configuration
│   ├── patterns.sh       # Bash anti-pattern detection
│   └── session-message.txt  # Centralized session start message
├── claude/               # Claude Code hooks (bash)
│   ├── session-start.sh  # Reads from core/session-message.txt
│   ├── pre-tool-bash.sh
│   └── pre-tool-edit.sh
└── opencode/             # OpenCode plugin (TypeScript)
    └── homeboy-plugin.ts # Reads from ~/.config/homeboy/agent-message.txt
```

## Architecture Notes

**Centralized Session Message**: The session start message is defined once in `core/session-message.txt`. This is the single source of truth that both agents read from:
- **Claude Code**: Reads directly from `~/.claude/hooks/agent-hooks/core/session-message.txt` (installed alongside hooks)
- **OpenCode**: Reads from `~/.config/homeboy/agent-message.txt` (copied during setup since plugin is installed separately)

**Claude Code** uses separate bash scripts for each hook type, configured via `~/.claude/settings.json`. Hooks are installed to `~/.claude/hooks/agent-hooks/` with access to the `core/` directory.

**OpenCode** uses a single TypeScript plugin that exports multiple hook handlers, installed to `~/.config/opencode/plugins/`. Since the plugin is installed separately from the source, shared configuration is accessed from `~/.config/homeboy/`.

Both implementations provide identical functionality and error messages for seamless switching between agents.
