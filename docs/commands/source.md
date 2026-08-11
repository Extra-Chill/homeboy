# Source

Inspect a controller-local directory against the exact sealed source-package
scanner used by Lab staging, before starting an expensive workflow:

```sh
homeboy source package check --path ./workspace
```

The command is read-only. It does not create a run, runner job, workspace,
artifact, connection, or source transfer.

It returns the standard JSON envelope. `data.source_package` has schema
`homeboy/source-package-check/v1` and reports the package format, valid verdict,
accepted file count and bytes, deterministic digest, excluded entries, and typed
path-attributed failures. A rejected package still produces its verdict and exits
with status 1, which makes it suitable for shell and orchestration preflight.

The current package format accepts regular files and directories. Symlinks are
recorded as exclusions and remain blocking under the current policy; their targets
are never traversed. Other special files, unreadable paths, per-file size limits,
aggregate byte limits, and entry limits are also blocking.
