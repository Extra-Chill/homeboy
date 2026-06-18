# `homeboy config`

Manage global Homeboy configuration stored under the OS config directory as `~/.config/homeboy/homeboy.json`.

## Overview

The `homeboy config` command provides access to global configuration that controls default behaviors across all Homeboy operations. If no `homeboy.json` file exists, built-in defaults are used automatically.

## Subcommands

### `homeboy config show`

Display the current configuration (merged built-in defaults + file overrides).

```bash
homeboy config show              # Show merged config
homeboy config show --builtin    # Show only built-in defaults
```

### `homeboy config set`

Set a configuration value at a JSON pointer path. Creates `homeboy.json` if it doesn't exist.

```bash
homeboy config set [--string] <pointer> <value>
```

By default, `<value>` is parsed as JSON so arrays, objects, booleans, numbers, and `null` keep their native types. Use JSON string quoting like `'"codex"'`, or pass `--string` when you want Homeboy to store the provided value as a literal string.

**Examples:**

```bash
# Remove legacy SCP protocol flag
homeboy config set /defaults/deploy/scp_flags '[]'

# Add a custom version file pattern
homeboy config set /defaults/version_candidates/4 '{"file": "VERSION", "pattern": "(\\d+\\.\\d+\\.\\d+)"}'

# Change local file permissions
homeboy config set /defaults/permissions/local/file_mode '"g+r"'

# Store copied run artifacts in a repo- or agent-readable directory
homeboy config set /artifact_root '"~/Developer/.homeboy-artifacts"'

# Set an extension string setting without JSON quoting
homeboy config set /settings/wp_sample-runtime_provider codex --string
```

### `homeboy config remove`

Remove a configuration value at a JSON pointer path.

```bash
homeboy config remove <pointer>
```

**Example:**

```bash
# Remove a custom version candidate
homeboy config remove /defaults/version_candidates/4
```

### `homeboy config reset`

Delete `homeboy.json` and restore to built-in defaults.

```bash
homeboy config reset
```

### `homeboy config path`

Show the path to `homeboy.json` and whether it exists.

```bash
homeboy config path
```

## Configuration Schema

```json
{
  "artifact_root": null,
  "defaults": {
    "install_methods": {
      "homebrew": {
        "path_patterns": ["/Cellar/", "/homebrew/"],
        "upgrade_command": "brew update && brew upgrade homeboy",
        "list_command": "brew list homeboy"
      },
      "cargo": {
        "path_patterns": ["/.cargo/bin/"],
        "upgrade_command": "cargo install homeboy"
      },
      "source": {
        "path_patterns": ["/target/release/", "/target/debug/"],
        "upgrade_command": "git pull && cargo build --release"
      }
    },
    "version_candidates": [
      { "file": "Cargo.toml", "pattern": "version\\s*=\\s*\"(\\d+\\.\\d+\\.\\d+)\"" },
      { "file": "package.json", "pattern": "\"version\"\\s*:\\s*\"(\\d+\\.\\d+\\.\\d+)\"" },
      { "file": "composer.json", "pattern": "\"version\"\\s*:\\s*\"(\\d+\\.\\d+\\.\\d+)\"" },
      { "file": "style.css", "pattern": "Version:\\s*(\\d+\\.\\d+\\.\\d+)" }
    ],
    "deploy": {
      "scp_flags": ["-O"],
      "artifact_prefix": ".homeboy-",
      "default_ssh_port": 22
    },
    "permissions": {
      "local": {
        "file_mode": "g+rw",
        "dir_mode": "g+rwx"
      },
      "remote": {
        "file_mode": "g+w",
        "dir_mode": "g+w"
      }
    }
  }
}
```

## Configurable Behaviors

### Artifact Root

Controls where Homeboy copies persisted run artifacts from commands such as `bench`, `trace`, and `observe`.

- `artifact_root`: Optional directory path for copied artifacts. Defaults to an `artifacts` subdirectory under the machine-local data directory.
- CLI override: `homeboy --artifact-root <dir> ...`
- Environment override: `HOMEBOY_ARTIFACT_ROOT=<dir>`
- Config override: `homeboy config set /artifact_root '"~/Developer/.homeboy-artifacts"'`

Precedence is CLI flag, then environment variable, then global config, then the built-in default.

### Install Methods

Controls how Homeboy detects installation method and upgrades itself:

- `path_patterns`: Strings to match in the executable path
- `upgrade_command`: Shell command to run for upgrades
- `list_command`: Optional command to verify installation (Homebrew only)

### Version Candidates

List of files and regex patterns for auto-detecting version files:

- `file`: Filename to look for (e.g., `Cargo.toml`)
- `pattern`: Regex with capture group for version string

### Deploy Settings

- `scp_flags`: Flags passed to SCP (default: `["-O"]` for legacy protocol compatibility)
- `artifact_prefix`: Prefix for temporary deployment artifacts (default: `.homeboy-`)
- `default_ssh_port`: Default SSH port (default: `22`)

### Permissions

File permission modes applied during build and deploy:

- `local.file_mode`: chmod mode for local files before build
- `local.dir_mode`: chmod mode for local directories before build
- `remote.file_mode`: chmod mode for deployed files
- `remote.dir_mode`: chmod mode for deployed directories

## JSON Output

All subcommands return JSON in the standard envelope format:

```json
{
  "success": true,
  "data": {
    "command": "config.show",
    "config": { ... }
  }
}
```

## Related

- [upgrade](upgrade.md) - Uses install_methods configuration
- [version](version.md) - Uses version_candidates configuration
- [deploy](deploy.md) - Uses deploy and permissions configuration
