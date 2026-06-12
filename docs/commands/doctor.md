# `homeboy doctor`

Read-only local diagnostics for Homeboy-adjacent work.

## Synopsis

```sh
homeboy doctor <COMMAND>
```

## Subcommands

### `resources`

```sh
homeboy doctor resources
```

Reports current machine pressure and Homeboy-adjacent hot processes. This is the
same resource-policy signal Homeboy uses before hot commands such as benchmark,
trace, and runner-heavy workflows.

By default, process pressure includes Homeboy processes. Set
`HOMEBOY_DOCTOR_RESOURCE_PROCESS_MATCHES` to a comma-separated list of additional
process names or command substrings for environment-specific workloads.

## JSON output

Use the global `--output <PATH>` flag to persist the command-specific structured
JSON payload to disk in addition to stdout:

```sh
homeboy --output doctor-resources.json doctor resources
```

## Related

- [bench](bench.md)
- [trace](trace.md)
- [runner](runner.md)
