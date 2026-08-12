# Source

Inspect a controller-local directory against the exact sealed source-package
scanner used by Lab staging, before starting an expensive workflow:

```sh
homeboy source package check --path ./workspace
```

The command is read-only. It does not create a run, runner job, workspace,
artifact, connection, or source transfer.

It returns the standard JSON envelope. `data.source_package` has schema
`homeboy/source-package-check/v1` and reports the scanner's `accepted`,
`excluded`, and `blocked` result sets. An accepted result includes the exact
package format, file count, bytes, and deterministic digest that staging uses. A
blocked result instead includes `partial` diagnostic counts without a package
identity. It exits with status 1, which makes it suitable for shell and
orchestration preflight.

Package format and link handling are scanner-owned. The command reports the exact
v1 or v2 accepted, excluded, and blocked outcome supplied by the shared staging
scanner, without traversing excluded entries. Special files, unreadable paths,
per-file size limits, aggregate byte limits, and entry limits are blocking.
