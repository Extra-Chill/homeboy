# `homeboy upgrade`

## Synopsis

```sh
homeboy upgrade [OPTIONS]
```

## Description

Upgrades Homeboy to the latest version. The command auto-detects the installation method (Homebrew, Cargo, source build, or downloaded release binary) and runs the appropriate upgrade process.

By default, after a successful local upgrade, Homeboy also checks configured SSH runners and runs their configured Homeboy binary through the same upgrade path. This keeps lab runners from drifting behind the local CLI.

Runner sync uses this contract:

- Run each configured SSH runner through its configured `homeboy_path`, falling back to bare `homeboy` only when no path is configured.
- Sync every parent extension that has portable source metadata (`source_url` plus `source_revision`) to the runner.
- Isolate extension sync failures per extension so one failed extension does not prevent later extensions from being attempted.
- Report configured `homeboy_path` versus bare `homeboy` version drift when both can be observed.
- Report a stale connected runner daemon when the daemon session version no longer matches the configured runner executable version.

## Options

- `--check`: Check for updates without installing. Returns version information without making changes.
- `--force`: Force upgrade even if already at the latest version.
- `--no-restart`: Skip automatic restart after upgrade. Useful for scripted environments.
- `--skip-extensions`: Skip automatic extension updates.
- `--skip-runners`: Skip automatic configured runner upgrades.
- `--upgrade-runner`: Upgrade only the named configured runner. Repeat to target multiple runners.
- `--method`: Override install method detection (`homebrew|cargo|source|binary`).

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

Upgrade only a specific runner after the local upgrade:

```sh
homeboy upgrade --upgrade-runner lab
```

Upgrade locally without touching configured runners:

```sh
homeboy upgrade --skip-runners
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
- `restart_required`: Boolean indicating if a restart is needed (true only for source installs)
- `extensions_updated`: Extension upgrade entries when installed extensions were checked
- `extensions_skipped`: Extension IDs that could not be updated
- `projects_migrated`: Project config migration entries
- `runners_updated`: Runner upgrade entries for configured runners that completed successfully
- `runners_skipped`: Runner upgrade entries for configured runners that failed or could not verify a version

Runner upgrade entries include the configured `homeboy_path`, observed configured executable version, optional bare `homeboy` version, optional path drift detail, per-extension sync successes/failures, and optional stale-daemon remediation commands.

## Exit code

- `0`: Success (upgrade completed or already at latest)
- Non-zero: Error during upgrade process

## Notes

- The `update` command is an alias for `upgrade` with identical behavior.
- Version checking queries the crates.io API. Network failures are handled gracefully.
- On Unix platforms, successful source installs automatically restart into the new binary. Binary and package-manager installs do not require a restart.

## Related

- [version](version.md)
- [extension](extension.md)
