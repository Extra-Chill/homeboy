# `homeboy extension`

## Synopsis

```sh
homeboy extension <COMMAND>
```

## Subcommands

### `list`

```sh
homeboy extension list [-p|--project <project_id>] [--live-readiness]
```

### `show`

```sh
homeboy extension show <extension_id> [--live-readiness]
```

Print detailed manifest, runtime, capability, and readiness information for one installed extension.

### Notification transport discovery

`extension list` includes each extension's compact `notification_transports`
inventory. `extension show <extension_id>` provides the same declared transport
`id` and `schema` for one extension. These read-only outputs deliberately omit
the transport command argv.

Use the discovered ID as the route-less default or pair it with an explicit
route for a schedule:

```sh
homeboy extension list
homeboy config set /notifications/default_transport '"<transport-id>"'
homeboy schedule add --notification-transport <transport-id> --notification-route '<transport-route>' ...
```

### Live readiness is explicit

`list` and `show` read installed manifests and cached readiness without spawning
extension processes. Pass `--live-readiness` to run each extension's
operator-authored `ready_check`, such as a WordPress doctor, npm probe, or
Codebox health script, and refresh the cached result.

Every live `ready_check` is bounded at 30s, overridable with
`HOMEBOY_EXTENSION_READY_CHECK_TIMEOUT_SECONDS`. A probe that hits the bound
has its process group terminated and reports `ready_reason:
"ready_check_timeout"`; the surrounding metadata is still returned. The bound
is per extension, not aggregate: live inventory over *n* extensions is bounded
by *n* times the per-check budget (#10517).

### Invalid installed manifests

`extension list` reports every discoverable extension directory, including an
installed manifest that cannot be loaded. Broken entries have `ready: false`
and include `id`, `path`, `manifest_path`, `error`, and `diagnostic`. `error`
is a machine-readable category such as `manifest_json_malformed`,
`manifest_deserialize_incompatible`, or `manifest_validation_incompatible`.
The diagnostic describes the failure without echoing manifest values. `extension
show <id>` returns the same fields in its structured command error so a broken
installed extension is distinguishable from an absent extension.

### `run`

```sh
homeboy extension run <extension_id> [-p|--project <project_id>] [-c|--component <component_id>] [-i|--input <key=value>]... [--stream|--no-stream] [<args...>]
```

- `--project` is required when the extension needs project context.
- `--component` is required when component context is ambiguous.
- `--input` repeats; each value must be in `KEY=value` form.
- `--stream` forces streaming output directly to terminal.
- `--no-stream` disables streaming and captures output.
- By default, Homeboy auto-detects streaming behavior based on TTY.
- Trailing `<args...>` are passed to CLI-type extensions.
- Safety manifest metadata marks `extension run` as an operator command because
  extension-owned runtime commands and forwarded arguments may mutate the target
  system.

### `set`

```sh
homeboy extension set [extension_id] --json <JSON> [--replace <field>]...
homeboy extension set [extension_id] --json '<JSON>'
```

Updates an extension manifest by merging a JSON object into the extension config.

Options:

- `--json <JSON>`: JSON object to merge into config (supports `@file` and `-` for stdin)
- `--replace <field>`: replace array fields instead of union (repeatable)

Notes:

- Use `null` in JSON to clear a field (for example, `{"commands": null}`).

### `setup`

```sh
homeboy extension setup <extension_id>
```

### `install`

```sh
homeboy extension install <source> [--id <extension_id>] [--ref <git-ref>] [--replace]
```

Installs an extension into Homeboy's extensions directory.

- If `<source>` is a git URL, Homeboy clones it and writes `sourceUrl` into the installed extension's `<extension_id>.json` manifest.
- For git URL installs, `--ref` checks out a branch, tag, or commit after cloning. The installed metadata still records the resolved `source_revision` SHA.
- If `<source>` is a local path, Homeboy symlinks the directory into the extensions directory.
- By default, install refuses to overwrite an existing extension. Use `--replace` to explicitly replace an existing install or link.

### `install-for-component`

```sh
homeboy extension install-for-component --source <source> [--path <component_path>]
```

Installs every extension configured by a component.

- `--source <source>`: git URL or local path to the extension repository or directory.
- `--path <component_path>`: component path containing `homeboy.json` (defaults to the current directory).

### `relink`

```sh
homeboy extension relink <extension_id> <source>
```

Repoints an existing symlinked extension to a new local source path. This command only repairs linked extensions; use `install --replace` for copied or cloned installs.

### `dev-run`

```sh
homeboy extension dev-run <extension_id> --source <path> --runner <runner_id> -- <command...>
```

Rapid iteration flow for extension authors. Homeboy snapshots the local extension source to the runner, refreshes the runner-side extension install from that synced path, then executes the provided command on the runner.

- Uses runner workspace sync safety for source materialization.
- Runs `homeboy extension refresh <remote_source> --id <extension_id>` on the runner before the requested command.
- Sets `HOMEBOY_EXTENSION_DEV_RUN_PROVENANCE_JSON` for the refresh and requested command.
- Leaves the runner extension refreshed/linked to the synced source path and reports the previous probe plus persistent state in JSON output.

### `update`

```sh
homeboy extension update <extension_id>
```

Updates a git-cloned extension.

- If the extension is symlinked, Homeboy returns an error (linked extensions are updated at the source directory).
- Update runs without an extra confirmation flag.
- By default, update runs against the local installed extension even when a preferred Lab runner is configured.
- To update the extension installed on a runner, pass explicit Lab intent with the global runner flag, for example `homeboy --runner <runner-id> extension update <extension_id>`.
- Homeboy reads `sourceUrl` from the extension's manifest to report the extension URL in JSON output.

### `converge`

```sh
homeboy extension converge
```

Refreshes installed extensions without replacing the controller binary. It does
not request controller-upgrade admission, so active controller ownership cannot
block compatible extension refresh.

- Preflights installed extension compatibility against the running controller.
- Preserves dirty sources: each dirty extension is reported in `skipped` with its
  exact dirty-path blocker; convergence does not force, stash, reset, or discard
  user changes.
- Validates the refreshed manifest before provider-catalog refresh or service
  restart. Linked sources that fail post-refresh validation or setup return to
  their prior clean revision.
- `revision_evidence` distinguishes proven `changed` revisions from `unchanged`
  and `unknown`; only proven changes can restart services.
- Reports bounded provider catalog diagnostics before and after refresh.
- Restarts only configured `resident_services` whose `extension_ids` include an
  extension with a proven changed revision. Services without `extension_ids`
  remain controller-binary services and are not restarted.

### `uninstall`

```sh
homeboy extension uninstall <extension_id>
```

Uninstalls an extension.

- If the extension is **symlinked**, Homeboy removes the symlink (the source directory is preserved).
- If the extension is **git-cloned**, Homeboy deletes the extension directory.

### `action`

```sh
homeboy extension action <extension_id> <action_id> [-p|--project <project_id>] [--data <json>]
```

Executes an action defined in the extension manifest.

- For `type: "api"` actions, `--project` is required.
- `--data` accepts a JSON array string of selected result rows (passed through to template variables like `{{selected}}`).

### `exec`

```sh
homeboy extension exec <extension_id> [-c|--component <component_id>] -- <command...>
```

Runs a tool from the extension's vendor/runtime directory. When `--component` is provided, the command runs with that component's path as the working directory. Safety manifest metadata marks `extension exec` as an operator command because forwarded commands may mutate the target system.

## Settings

Homeboy builds an **effective settings** map for each extension by merging settings across scopes, in order (later scopes override earlier ones):

1. Project (`projects/<project_id>.json`): `extensions.<extension_id>.settings`
2. Component (`components/<component_id>.json`): `extensions.<extension_id>.settings`

When running an extension, Homeboy passes an execution context via environment variables:

- `HOMEBOY_EXEC_CONTEXT_VERSION`: currently `2`
- `HOMEBOY_EXTENSION_ID`
- `HOMEBOY_SETTINGS_JSON`: merged effective settings (JSON)
- `HOMEBOY_PROJECT_ID` (optional; when a project context is used)
- `HOMEBOY_EXTENSION_PATH`: absolute path to extension directory
- `HOMEBOY_PROJECT_PATH` (optional; absolute path to project directory)
- `HOMEBOY_COMPONENT_ID` (optional; when a component context is resolved)
- `HOMEBOY_COMPONENT_PATH` (optional; absolute path to component directory)
- `HOMEBOY_STEP` / `HOMEBOY_SKIP` (optional; comma-separated step filters)

Extensions can define additional environment variables via `runtime.env` in their manifest.

`homeboy extension run` and `extension.run` pipeline steps share the same execution core (template vars, settings JSON, and env handling). Both paths keep the same CLI output contract while sharing internal execution behavior.

Extension settings validation happens during extension execution and may also be checked by other commands.

`homeboy extension run` requires the extension to be installed or linked under
the Homeboy extensions directory, discovered by scanning
`~/.config/homeboy/extensions/<extension_id>/<extension_id>.json`. There is no
separate `installedModules` requirement in global config.

## Runtime Configuration

Executable extensions define their runtime behavior in their extension manifest (`extensions/<extension_id>/<extension_id>.json`):

```json
{
  "runtime": {
    "run_command": "./venv/bin/python3 {{entrypoint}} {{args}}",
    "setup_command": "python3 -m venv venv && ./venv/bin/pip install -r requirements.txt",
    "ready_check": "test -f ./venv/bin/python3",
    "entrypoint": "main.py",
    "env": {
      "MY_VAR": "{{extensionPath}}/data"
    }
  }
}
```

- `run_command`: Shell command to execute the extension. Template variables: `{{extensionPath}}`, `{{entrypoint}}`, `{{args}}`, plus project context vars.
- `setup_command`: Optional shell command to set up the extension (run during install/update).
- `ready_check`: Optional shell command to check if extension is ready (exit 0 = ready). Bounded at 30s per invocation; override with `HOMEBOY_EXTENSION_READY_CHECK_TIMEOUT_SECONDS`.
- `env`: Optional environment variables to set when running.

## Release Configuration

Release steps can be backed by extension actions named `release.<step_type>`.

## JSON output

> Note: all command output is wrapped in the global JSON envelope described in the [JSON output contract](../architecture/output-system.md). `homeboy extension` returns a tagged `ExtensionOutput` object as `data`.

Top-level variants (`data.command`):

- `extension.list`: `{ project_id?, extensions: ExtensionEntry[] }`
- `extension.show`: `{ extension: ExtensionDetail }`
- `extension.run`: `{ extension_id, project_id? }`
- `extension.setup`: `{ extension_id }`
- `extension.install`: `{ extension_id, source, path, linked }`
- `extension.replace`: `{ extension_id, old_path, new_path, source, linked, source_revision? }`
- `extension.update`: `{ extension_id, url, path }`
- `extension.update_all`: `{ updated: UpdateEntry[], skipped: string[] }`
- `extension.uninstall`: `{ extension_id, path, was_linked }`
- `extension.action`: `{ extension_id, action_id, project_id?, response }`
- `extension.exec`: `{ extension_id, exit_code?, stdout?, stderr? }`
- `extension.set`: `{ extension_id, updated_fields }` or `{ batch }` for JSON batch updates

Extension entry (`extensions[]`):

- `id`, `name`, `version`, `description`
- `runtime`: `executable` (has runtime config) or `platform` (no runtime config)
- `compatible` (with optional `--project`)
- `ready` (runtime readiness based on `ready_check`; see `ready_reason` for
  `ready_check_skipped` / `ready_check_timeout` / `ready_check_reentrant_skipped`,
  each of which means the check did not produce a live pass/fail)
- `linked`: whether the extension is symlinked
- `path`: extension directory path (may be empty if unknown)
- Optional fields include `ready_reason`, `ready_detail`, `manifest_path`, `error`, `diagnostic`, `symlink_target`, `source_revision`, `cli_tool`, `cli_display_name`, `actions`, `notification_transports`, `has_setup`, and `has_ready_check`.

Extension detail (`extension.show`):

- `structured_sidecars`: declared structured sidecar contracts, each with `name`, `path`, and `schema_version`
- `notification_transports`: declared transport descriptors with `id` and `schema`; command argv is not exposed

## Exit code

- `extension.run`: exit code of the executed extension's `run_command`.
- `extension.setup`: `0` on success; if no `setup_command` is defined, returns `0` without action.

## Extension-provided commands and docs

Extensions can provide their own top-level CLI commands and documentation topics.

Root help lists installed extension commands alongside the built-in ones, plus an
`Extension-provided commands:` summary and any broken-link warnings:

```sh
homeboy --help
```

Discover what’s available on your machine:

```sh
homeboy self docs list
```

Render an extension-provided topic:

```sh
homeboy self docs <topic>
```

Because extension commands and docs are installed locally, the core CLI documentation stays focused on the extension system rather than any specific extension-provided commands.

## Related

- [self](self.md)
- [project](project.md)
