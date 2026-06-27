# `homeboy self`

Inspect the active Homeboy binary and nearby install/update signals.

## Synopsis

```sh
homeboy self <COMMAND>
```

## Subcommands

- `status` — report the active binary, version, and install/update signals
- `identity` — report the active binary build identity without external probes
- `doctor` — report one authoritative binary/runtime view, command-surface drift checks, and host resource pressure
- `cleanup-runtime-tmp` — plan or delete orphaned Homeboy runtime temp entries

### `status`

```sh
homeboy self status
```

Reports active binary location, version, build identity, and nearby install or
update signals.

### `identity`

```sh
homeboy self identity
```

Returns the current binary build identity directly from the running executable.
Use this when a runner or daemon freshness check needs a cheap local identity
without probing surrounding install state.

### `doctor`

```sh
homeboy self doctor
```

Reports one authoritative runtime view spanning the controller and every
configured runner so operators never have to manually reason about which
Homeboy binary is in effect. The controller is the authoritative reference
point; each runner row reports its configured executable path, the version and
build identity of its active daemon (when connected), and how that compares to
the controller.

The report also includes a read-only `command_surface` section that compares the
in-process source command registry, the docs command index, and the help-facing
top-level command names. Runtime extension docs such as `cargo` and `wp` are
reported separately so stale core entries still stand out without requiring an
extension to be installed.

When every participant agrees with the controller, `agrees` is `true` and the
command exits `0`. When any runner reports a different version or a stale daemon
(a daemon started by a different build than the configured runner executable),
`agrees` is `false`, the disagreement is described in `drift_notes`, and the
command exits non-zero so cook loops can detect binary-identity or command
surface drift.

The report also carries a `resources` section with read-only host diagnostics —
machine load relative to CPU count, memory pressure, the hottest
Homeboy-adjacent processes, and active rig run leases — with an overall
`recommendation` of `ok`, `warm`, or `hot`. Resource pressure is diagnostic
context only and does not affect the `agrees` exit code. This consolidates host
and resource diagnostics under `self`; there is no standalone `doctor` command.

### `cleanup-runtime-tmp`

```sh
homeboy self cleanup-runtime-tmp [--older-than-days <days>] [--prefix <prefix>] [--limit <n>] [--apply]
```

Plans cleanup for orphaned Homeboy runtime temp entries. Without `--apply`, this
is a dry run. Pass `--apply` to delete the planned entries.

- `--older-than-days <days>`: only include entries older than this many days; defaults to `7`.
- `--prefix <prefix>`: only include entries whose file or directory name starts with the prefix.
- `--limit <n>`: maximum temp entries to inspect; defaults to `1000`.
- `--apply`: delete planned entries instead of only reporting the plan.

## Related

- [upgrade](upgrade.md)
- [daemon](daemon.md)
