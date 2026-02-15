# `homeboy upgrade`

## Synopsis

```sh
homeboy upgrade [OPTIONS]
homeboy update [OPTIONS]  # alias
```

## Description

Upgrades Homeboy to the latest version. The command auto-detects the installation method (Homebrew, Cargo, source build, or downloaded release binary) and runs the appropriate upgrade process.

By default, after a successful upgrade, Homeboy restarts itself to use the new version.

## Options

- `--check`: Check for updates without installing. Returns version information without making changes.
- `--force`: Force upgrade even if already at the latest version.
- `--no-restart`: Skip automatic restart after upgrade. Useful for scripted environments.
- `--method`: Override install method detection (homebrew|cargo|source|binary).

## Installation Method Detection

Homeboy detects how it was installed and uses the appropriate upgrade method:

| Method | Detection | Upgrade Command |
|--------|-----------|-----------------|
| Homebrew | Binary path contains `/Cellar/` or `/homebrew/`, or `brew list homeboy` succeeds | `brew update && brew upgrade homeboy` |
| Cargo | Binary path contains `/.cargo/bin/` | `cargo install homeboy` |
| Source | Binary path contains `/target/release/` or `/target/debug/` | `git pull && cargo build --release` |
| Binary | Binary path contains `/bin/homeboy` (covers `~/bin/homeboy` and `/usr/local/bin/homeboy`) | Downloads latest release asset and replaces the current binary |

If the installation method cannot be detected, an error is returned with manual upgrade instructions. You can also override detection:

```sh
homeboy upgrade --method binary
```

## Examples

Check for updates:

```sh
homeboy upgrade --check
```

Upgrade to the latest version:

```sh
homeboy upgrade
```

Upgrade without auto-restart:

```sh
homeboy upgrade --no-restart
```

Force reinstall:

```sh
homeboy upgrade --force
```

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md).

`homeboy upgrade --check` data payload:

- `command`: `upgrade.check`
- `current_version`: Current installed version
- `latest_version`: Latest available version from crates.io (may be null if network fails)
- `update_available`: Boolean indicating if an update is available
- `install_method`: Detected installation method (`homebrew`, `cargo`, `source`, or `unknown`)

`homeboy upgrade` data payload:

- `command`: `upgrade`
- `install_method`: Installation method used for upgrade
- `previous_version`: Version before upgrade
- `new_version`: Version after upgrade (may be null)
- `upgraded`: Boolean indicating if upgrade was performed
- `message`: Human-readable status message
- `restart_required`: Boolean indicating if a restart is needed

## Exit code

- `0`: Success (upgrade completed or already at latest)
- Non-zero: Error during upgrade process

## Notes

- The `update` command is an alias for `upgrade` with identical behavior.
- Version checking queries the crates.io API. Network failures are handled gracefully.
- On Unix platforms, successful upgrades automatically restart into the new binary.
- On non-Unix platforms, Homeboy prints a message to restart manually.

## Related

- [version](version.md)
- [module](module.md)
