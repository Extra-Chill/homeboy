# OpenClaw Module for Homeboy

Homeboy module for managing [OpenClaw](https://github.com/openclaw/openclaw) AI agent installations.

## What It Does

- **Discovery** — Auto-detects OpenClaw installations on a server
- **Config Visibility** — Pins gateway config, workspace files (SOUL.md, USER.md, MEMORY.md, etc.)
- **Log Access** — Pins gateway and agent logs
- **Actions** — Gateway status, restart, config view, cron listing, agent listing
- **CLI Passthrough** — `homeboy openclaw <agent> <command>` wraps the OpenClaw CLI

## Install

```bash
homeboy module install https://github.com/Extra-Chill/homeboy-module-openclaw
```

Or from a local path:

```bash
homeboy module install /path/to/openclaw-module
```

## Usage

### CLI

```bash
# Check gateway status
homeboy openclaw my-agent gateway status

# View config
homeboy openclaw my-agent config get

# List cron jobs
homeboy openclaw my-agent cron list
```

### Actions (Desktop App)

```bash
# Run a module action
homeboy module action openclaw gateway-status -p my-agent
homeboy module action openclaw config-get -p my-agent
homeboy module action openclaw cron-list -p my-agent
```

### Pinned Files

The module automatically pins these workspace files for viewing in the desktop app:

| File | Purpose |
|------|---------|
| `openclaw.json` | Gateway configuration |
| `workspace/SOUL.md` | Agent personality & identity |
| `workspace/USER.md` | User context & preferences |
| `workspace/AGENTS.md` | Agent instructions |
| `workspace/TOOLS.md` | Tool configuration |
| `workspace/MEMORY.md` | Long-term memory |
| `workspace/HEARTBEAT.md` | Periodic task config |
| `workspace/IDENTITY.md` | Agent identity metadata |

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `profile` | (blank) | OpenClaw profile name. Blank = default (`~/.openclaw`) |
| `gateway_port` | `19000` | Gateway daemon port |

## Fleet Integration

Works with Homeboy's fleet system. Add OpenClaw agents as projects, group them into fleets:

```bash
# Register an agent as a project
homeboy project add star-fleet-command --module openclaw --server my-vps

# Fleet operations
homeboy fleet create ai-agents --projects star-fleet-command,sarai-chinwag
homeboy fleet status ai-agents
```
