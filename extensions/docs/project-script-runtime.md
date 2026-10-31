# Project Script Runtime Helpers

`scripts/lib/project-scripts.sh` is the generic project-root, package-manager,
and project-script helper for Homeboy extension runners. It is intentionally kept
in Homeboy Extensions while Homeboy core only dispatches extension capabilities.

## Contract

Extension scripts source the helper and initialize it for the ecosystem they are
about to run:

```bash
source "$HOMEBOY_EXTENSION_PATH/scripts/lib/project-scripts.sh"
homeboy_project_init --ecosystem node --path "$PROJECT_PATH"
```

The helper sets these generic variables:

| Variable | Meaning |
| --- | --- |
| `HOMEBOY_PROJECT_ECOSYSTEM` | Ecosystem adapter selected by the extension. |
| `HOMEBOY_PROJECT_ROOT` | Root discovered from the input path. |
| `HOMEBOY_PROJECT_SCRIPT_FILE` | Script manifest path, such as `package.json`. |
| `HOMEBOY_PROJECT_PACKAGE_MANAGER` | Ecosystem package manager selected from project files. |
| `HOMEBOY_PROJECT_RUN_CMD` | Command prefix for running named project scripts. |
| `HOMEBOY_PROJECT_EXEC_CMD` | Command prefix for running ecosystem binaries. |

The helper exposes these generic functions:

| Function | Purpose |
| --- | --- |
| `homeboy_project_init --ecosystem <id> --path <path>` | Select an ecosystem adapter and discover the project root. |
| `homeboy_project_has_script <name>` | Return success when the named project script exists. |
| `homeboy_project_run_script_command <name>` | Print the shell command prefix for the named script. |
| `homeboy_project_run_script <name> [args...]` | Run a named project script from `HOMEBOY_PROJECT_ROOT`. |
| `homeboy_project_exec_command <binary> [args...]` | Print the shell command prefix for an ecosystem binary. |
| `homeboy_project_exec <binary> [args...]` | Run an ecosystem binary from `HOMEBOY_PROJECT_ROOT`. |
| `homeboy_project_ensure_dependencies` | Install dependencies for runner snapshots when supported. |

## Current Adapters

The current adapters are declared in
[`../dependency-adapters/examples/`](../dependency-adapters/examples/). The
helper loads those manifests to discover project roots, select package managers,
build script/exec commands, and choose install commands.

`node` / `nodejs` finds the nearest `package.json` at or above the input path and
selects the package manager from committed project signals in manifest priority
order:

1. `pnpm-lock.yaml` -> `pnpm`
2. `yarn.lock` -> `yarn`
3. otherwise -> `npm`

Lockfiles are authoritative even when the corresponding executable is missing on
the current host. That keeps discovery deterministic; execution then fails with a
normal missing-tool error instead of silently running the wrong package manager.

The pnpm adapter uses manifest-declared upward search so a package inside a pnpm
workspace runs scripts from the package root while installing dependencies from
the workspace root.

`composer` / `php` finds `composer.json`, exposes `composer run-script` and
`composer exec`, and materializes `vendor/` from the Composer adapter manifest.

`wordpress` requires the manifest-declared WordPress root signals
(`wp-content` and `wp-includes`) and composes helper behavior from the Composer
and Node.js adapters when `composer.json` or `package.json` are present. The
WordPress adapter describes the helper surface without embedding package-manager
behavior in Homeboy core.

## Homeboy Core Seam

This helper is the extension-owned implementation seam for a future Homeboy core
runtime helper. A core contract can expose the same concepts without taking an
ecosystem dependency:

1. project root discovery result
2. script manifest path
3. package/tool runner command descriptors
4. named-script existence checks
5. dependency preparation hook

Until that contract exists in Homeboy core, generic script execution belongs here
so core primitives stay ecosystem-agnostic.
