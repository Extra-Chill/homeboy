# Plasma Shield Module

Network security control for AI agent fleets.

## Two Interfaces

**Desktop App** — Visual dashboard in Homeboy Desktop sidebar. Select views, see results in tables.

**CLI** — Command-line via `homeboy plasma <command>`. Full control from terminal.

## Overview

This module provides CLI integration with [Plasma Shield](https://github.com/Extra-Chill/plasma-shield) routers, enabling fleet-wide security management from the command line.

## Installation

```bash
# Clone and symlink
git clone https://github.com/Extra-Chill/homeboy-modules.git
ln -s /path/to/homeboy-modules/plasma-shield ~/.config/homeboy/modules/plasma-shield
```

## Configuration

Routers are stored in `~/.config/homeboy/plasma-shield/routers.json`:

```json
{
  "local": {
    "api_url": "http://127.0.0.1:9000",
    "api_key": null,
    "description": "Local development shield"
  },
  "prod": {
    "api_url": "http://shield.example.com:9000",
    "api_key": "your-api-key",
    "description": "Production shield router"
  }
}
```

## Usage

```bash
# Router management
homeboy plasma routers                    # List configured routers
homeboy plasma router add prod http://shield.example.com:9000 "Production" api-key
homeboy plasma router remove old-router

# Status
homeboy plasma status                     # Default router status
homeboy plasma prod status                # Specific router status

# Mode control
homeboy plasma mode                       # Get current mode
homeboy plasma mode enforce               # Set global mode
homeboy plasma mode audit                 # Set audit mode (log only)
homeboy plasma mode lockdown              # Block ALL traffic

# Agent-specific control
homeboy plasma agent sarai lockdown       # Lockdown specific agent
homeboy plasma agent sarai audit          # Set agent to audit mode
homeboy plasma agent sarai clear          # Clear override (use global)

# Traffic logs
homeboy plasma logs                       # Last 50 entries
homeboy plasma logs --limit 100           # Custom limit
homeboy plasma logs --follow              # Stream logs
homeboy plasma logs --agent sarai         # Filter by agent

# Blocking rules
homeboy plasma rules                      # List rules
homeboy plasma rules list
homeboy plasma rules add --domain evil.com --action block --desc "Block evil"
homeboy plasma rules remove rule-id
```

## Modes

| Mode | Behavior |
|------|----------|
| `enforce` | Block requests matching rules (default) |
| `audit` | Log all traffic, never block (testing) |
| `lockdown` | Block ALL traffic (emergency) |

## Requirements

- `jq` for JSON parsing
- `curl` for API requests
- Access to a Plasma Shield router

## Shield Router Setup

See [Plasma Shield](https://github.com/Extra-Chill/plasma-shield) for router installation and configuration.
