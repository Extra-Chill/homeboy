# `homeboy upgrade`

## Synopsis

```sh
homeboy upgrade [OPTIONS]
```

## Description

Upgrades Homeboy to the latest version. The command auto-detects the installation method (Homebrew, source build, or downloaded release binary) and runs the appropriate upgrade process.

By default, after a successful local upgrade, Homeboy also converges configured SSH runners on the installed controller release. This keeps lab runners from drifting behind the local CLI.

### Runner convergence

Each selected SSH runner goes through the same runner-binary convergence as `homeboy runner connect` (see [runner connection](../architecture/runner-connection.md#binary-convergence)). The target is the controller identity the upgrade just installed, not the build of the process running the upgrade. Convergence compares the runner's configured `homeboy_path` (its `self identity`) with that target and installs the target's exact release tag through `refresh-homeboy`'s verified release path only when the runner is older. It never downgrades, and it never replaces a daemon that has active jobs.

Outside an upgrade, `homeboy runner connect <runner>` runs this same convergence against the current controller, so it is the normal way to bring a single runner up to date. `homeboy runner refresh-homeboy` is for explicit refs (`--ref`), selecting an existing binary (`--select`), downgrades (`--allow-downgrade`), dev builds (`runner dev-sync`), and the manual fallback below when convergence failed.

Each convergence outcome maps onto the runner's upgrade entry like this:

| Convergence status | Entry |
| --- | --- |
| `converged` | `success: true`, `upgraded: true`, `new_version` is the target version |
| `already_current` | `success: true`, `upgraded: false`; nothing is installed |
| `pending_active_jobs` | `deferred: true`; `detail` names the blocking job IDs; recover with `homeboy upgrade --upgrade-runner <runner>` once they finish |
| `failed`, `skipped_unverifiable` | `success: false`; `detail` keeps the attributed cause; recover with `homeboy runner refresh-homeboy <runner> --ref v<target> --reconnect` |
| `skipped_remote_newer`, `skipped_dev_build` | `success: false`, with no recovery command (the runner is never downgraded, and a dev build has no published release) |

Only a `converged` or `already_current` binary continues to:

- sync every parent extension that has portable source metadata (`source_url` plus `source_revision`) to the runner, isolating failures per extension;
- report a stale connected runner daemon (`stale_daemon`) whose session version does not match the target;
- reconcile the runner-service unit binaries (`service_binaries`), restarting this controller's stale unit through the idle-gated `refresh-homeboy --reconnect` handoff.

Controller upgrades install the verified binary, stop the resident daemon by
its exact lease, and start the installed binary in the same state directory.
Startup recovery resumes in-flight work; upgrades do not drain, cancel, or
block new Cook admission. A candidate with a different observation-store schema
is refused while non-terminal work exists, naming that work and leaving all
binaries unchanged.

## Options

- `status [ID]`: Inspect a persisted upgrade operation. Reports whether binary promotion and optional extension/runner refresh completed. Omit `ID` to read the latest upgrade run. Also available as `homeboy runs show <id>`.
- `--check`: Check for updates without installing. Returns version information without making changes.
- `--force`: Force upgrade even if already at the latest version; does not cancel active work.
- `--skip-extensions`: Skip automatic extension updates.
- `--skip-runners`: Upgrade the controller only; cannot be combined with `--upgrade-runner`.
- `--upgrade-runner`: Select configured runners to converge with the controller upgrade. Repeat to target multiple runners.
- `--runner-only`: With `--upgrade-runner`, preserve the controller and refresh only the selected runners. The response explicitly reports this partial scope and the exact controller-plus-runner convergence command.
- `--method`: Override install method detection (`homebrew|source|binary`).
- `--version <TAG>`: Pin the published release to install (`v0.332.0` or `0.332.0`). When `--method` is omitted, this infers `--method binary` and reports that choice before mutation. An explicit incompatible method is rejected. A pin also implies `--force` at the release gate so a deliberate downgrade is not discarded as "already at latest".

## Release selection for binary installs

Binary installs download a release asset named `homeboy-<target triple>.tar.xz`, plus its `.sha256` sidecar. A release published without this platform's pair cannot be installed here, so `upgrade` does not simply resolve the newest tag:

- **`--check` verifies an installable artifact**, not just a newer version string. `latest_version` is the newest release carrying an artifact for the running target, and `update_available` is keyed off it. A release this platform cannot install is never reported as available.
- **Selection falls back** to the newest release that does have this target's asset, and says so: `v0.333.0 has no x86_64-unknown-linux-gnu asset; upgrading to v0.332.0 instead.`
- **`--version <TAG>` pins deliberately.** With no explicit `--method`, a pin selects the binary install method even when the current controller was installed by Cargo. A pin to a release with no asset for this target is refused before any binary mutation, naming the asset it looked for and the releases that would work.
- **The target triple is detected honestly.** An OS/architecture pair Homeboy publishes no assets for is reported as undetermined; asset availability is then explicitly *not verified* rather than guessed.
- **A 404 names the resolved asset URL and the target triple** it looked for, so diagnosing a missing asset does not require listing release assets by hand.

Binary upgrades run the same `homeboy-installer.sh` that is published as a release asset, with `HOMEBOY_INSTALL_STAGE_ONLY=1` and `HOMEBOY_INSTALL_PATH` pointing into an isolated stage. In that mode the installer only downloads, checksum- and version-verifies, and places the candidate in the stage; Homeboy then verifies the staged candidate, queries its observation-store schema before replacement, and atomically promotes it over the active binary. The installer honors `HOMEBOY_UPGRADE_RELEASE_TAG` to download from a specific tag instead of `releases/latest/download`; `upgrade` sets it from the release it selected.

Controllers released before the bootstrap admission contract can recover without
running their legacy `upgrade` command by invoking the pinned release installer:

```sh
curl -fsSL https://github.com/Extra-Chill/homeboy/releases/download/v<fixed-version>/homeboy-installer.sh | sh
```

The installer stages the candidate, has that candidate read durable ownership
with the legacy controller identity, and only replaces the installed binary when
the candidate admission succeeds.

## Installation Method Detection

Homeboy detects how it was installed and uses the appropriate upgrade method:

| Method | Detection | Upgrade Command |
|--------|-----------|-----------------|
| Homebrew | Binary path contains `/Cellar/` or `/homebrew/`, or `brew list homeboy` succeeds | `brew update && brew upgrade homeboy` |
| Source | Binary path contains `/target/release/` or `/target/debug/` | `git pull && cargo build --release` |
| Binary | Binary path contains `/bin/homeboy` (covers `~/bin/homeboy`, `/usr/local/bin/homeboy`, and `~/.cargo/bin/homeboy`) | Runs `homeboy-installer.sh` in stage-only mode, then verifies, admits, and promotes the staged release over the current binary |

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

Force reinstall:

```sh
homeboy upgrade --force
```

Pin a specific published release (infers a binary install):

```sh
homeboy upgrade --version v0.332.0
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
- `latest_version`: The release an upgrade would install. For asset-installed methods this is the newest release carrying an artifact for `target`. Null when the network check failed, or when no published release ships this target's artifact
- `update_available`: Boolean indicating if an installable update is available
- `install_method`: Detected installation method (`homebrew`, `binary`, `source`, or `unknown`)
- `target`: Running target triple. Omitted when the OS/architecture pair could not be identified, which means asset availability was *not verified*
- `newest_version`: Newest published release, whatever its asset inventory. Equal to `latest_version` on a healthy release train
- `uninstallable_versions`: Releases newer than `latest_version` passed over for want of a `target` asset, newest first
- `notice`: Plain-language explanation when `latest_version` is not the newest release, when nothing installable was found, or when the target could not be determined

`homeboy upgrade status [ID]` data payload:

- `command`: `upgrade.status`
- `operation_id`: Persisted observation-run id
- `status`: Observation run status (`running`, `pass`, `error`, ...)
- `phase`: Current or terminal upgrade phase
- `elapsed_seconds`: Bounded elapsed time for the recorded phase
- `controller` / `extensions` / `runners`: Independent component statuses
- `note`: Present when a running record's owner process is no longer alive

Status reads reconcile an upgrade record whose owner has exited to
a non-success interrupted terminal state, projecting any pending binary
replacement from its durable checkpoint. A proven controller replacement
remains visible independently from incomplete extension or runner refresh.
Controller replacement does not wait for live runtime-generation pins.
Upgrades run in the foreground through binary promotion and daemon restart
resumption. A queued worker is not reported as successful completion. Only the
controller admission window is fenced while existing Cooks finish. Runner jobs
keep running and later complete through the upgraded controller. After the
controller upgrade, each selected runner converges on the installed release:
idle runners converge at once, and runners whose daemon has active jobs go in
`runners_deferred` with `runner busy: the daemon has active jobs (<job-ids>); ...`.
Deferred runners do not make the upgrade partial.

`homeboy upgrade` data payload:

- `command`: `upgrade`
- `operation_id`: Durable observation-run id persisted before mutation. Inspect later with `homeboy upgrade status <id>`
- `install_method`: Installation method used for upgrade
- `previous_version`: Version before upgrade
- `new_version`: Version after upgrade (may be null)
- `upgraded`: Boolean indicating if upgrade was performed
- `partial`: Present and true when a selected runner did not converge with the controller
- `runners_deferred`: Busy runners whose opportunistic refresh was deferred
- `message`: Human-readable status message
- `restart_required`: Boolean indicating if a restart is needed (true only for source installs)
- `extensions_updated`: Extension upgrade entries when installed extensions were checked
- `extensions_skipped`: Extension IDs that could not be updated
- `projects_migrated`: Project config migration entries
- `runners_updated`: Runner upgrade entries for configured runners that completed successfully
- `runners_skipped`: Runner upgrade entries for configured runners that failed or could not verify a version

Runner upgrade entries include the configured `homeboy_path`, observed configured executable version, optional bare `homeboy` version, optional path drift detail, per-extension sync successes/failures, and optional stale-daemon remediation commands.

## Exit code

- `0`: Completed upgrade with selected targets converged and runner admission restored.
- Non-zero: Schema-change refusal, preflight failure, partial upgrade, failed component, interrupted operation, pending service restart, or admission-readiness failure.

## Notes

- Version checking queries the GitHub Releases API. Network failures are handled gracefully.
- On Unix platforms, successful source installs automatically restart into the new binary. Binary and package-manager installs do not require a restart.
- Upgrade persists a durable operation before controller mutation. Long extension refresh emits bounded `[upgrade] phase=... elapsed=Ns` heartbeats. `homeboy upgrade status` reconciles dead foreground owners and reports whether the destination hash matches the selected release's durably recorded sha256; optional refresh is marked interrupted when its owner exits.

## Related

- [release](release.md)
- [extension](extension.md)
