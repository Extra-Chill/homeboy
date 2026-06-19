# `homeboy self`

Inspect the active Homeboy binary and nearby install/update signals.

## Synopsis

```sh
homeboy self <COMMAND>
```

## Subcommands

- `status` — report the active binary, version, and install/update signals
- `identity` — report the active binary build identity without external probes
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
