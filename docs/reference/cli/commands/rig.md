<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy rig` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/rig.md](../../../commands/rig.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy rig`

```sh
homeboy rig <COMMAND>
```

Manage local dev rigs (reproducible multi-component environments)

| Subcommand | Summary |
| --- | --- |
| `homeboy rig list` | List all declared rigs |
| `homeboy rig show` | Show a rig spec |
| `homeboy rig materialize` | Read and normalize a rig spec, resolving its local inheritance chain |
| `homeboy rig up` | Materialize a rig: run its `up` pipeline |
| `homeboy rig check` | Run a rig's `check` pipeline and report health |
| `homeboy rig lint` | Lint a rig package without touching the environment |
| `homeboy rig package` | Validate rig package artifacts without touching the environment |
| `homeboy rig down` | Tear down a rig: stop services and run its `down` pipeline |
| `homeboy rig repair` | Repair safe declared drift without running the full `up` pipeline |
| `homeboy rig sync` | Sync every stack declared by this rig's components |
| `homeboy rig run` | Refresh, sync, check, and benchmark a rig profile end-to-end |
| `homeboy rig status` | Show current state of a rig: running services, last up/check |
| `homeboy rig release-lock` | Release a stuck active-run lock (rig lease) so a new run can proceed |
| `homeboy rig install` | Install rigs from a local package path or git URL |
| `homeboy rig update` | Update rigs installed from git-backed rig packages |
| `homeboy rig sources` | Inspect or remove installed rig sources |
| `homeboy rig app` | Install, update, or remove this rig's desktop app launcher |
| `homeboy rig artifact` | Register local command-step evidence with the enclosing rig run |

## `homeboy rig list`

```sh
homeboy rig list
```

List all declared rigs

## `homeboy rig show`

```sh
homeboy rig show <RIG_ID>
```

Show a rig spec

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

## `homeboy rig materialize`

```sh
homeboy rig materialize [OPTIONS] <RIG_PATH>
```

Read and normalize a rig spec, resolving its local inheritance chain

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_PATH>` | yes | Path to a rig.json file |

| Option | Value | Description |
| --- | --- | --- |
| `--source-root` | `<SOURCE_ROOT>` | Directory that inherited templates must remain within. Defaults to the package root for rigs/<id>/rig.json, otherwise the rig directory |

## `homeboy rig up`

```sh
homeboy rig up [OPTIONS] <RIG_ID>
```

Materialize a rig: run its `up` pipeline

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Build an execution plan without running the rig |

## `homeboy rig check`

```sh
homeboy rig check [OPTIONS] <TARGET>
```

Run a rig's `check` pipeline and report health

| Argument | Required | Description |
| --- | --- | --- |
| `<TARGET>` | yes | Rig ID, local package path, or direct rig.json path |

| Option | Value | Description |
| --- | --- | --- |
| `--id` | `<ID>` | Select a rig from a local package path containing multiple rigs |
| `--path` | `<CHECKOUT>` | Override the rig's primary component checkout path for this check. Uses bench.default_component when present, otherwise the rig's only component |

## `homeboy rig lint`

```sh
homeboy rig lint [OPTIONS] <TARGET>
```

Lint a rig package without touching the environment.

Runs ONLY the env-independent package lint (conflict markers, JSON validity, and `extends` template materialization) — no requirements and no live `check` probes. This is the entry point CI uses to validate a rig package where no component checkouts exist.

| Argument | Required | Description |
| --- | --- | --- |
| `<TARGET>` | yes | Rig ID, local package path, or direct rig.json path |

| Option | Value | Description |
| --- | --- | --- |
| `--id` | `<ID>` | Select a rig from a local package path containing multiple rigs |
| `--all` | flag | Lint every rig discovered in a local package path |
| `--format` | `<FORMAT>` | Output format. `json` uses Homeboy's standard command-result envelope Values: `json`. |

## `homeboy rig package`

```sh
homeboy rig package <COMMAND>
```

Validate rig package artifacts without touching the environment

| Subcommand | Summary |
| --- | --- |
| `homeboy rig package lint` | Lint every rig and package-level manifest under a local package path |

## `homeboy rig package lint`

```sh
homeboy rig package lint <MANIFEST_PATH>
```

Lint every rig and package-level manifest under a local package path

| Argument | Required | Description |
| --- | --- | --- |
| `<MANIFEST_PATH>` | yes | Local package directory containing rig.json or rigs/<id>/rig.json |

## `homeboy rig down`

```sh
homeboy rig down [OPTIONS] <RIG_ID>
```

Tear down a rig: stop services and run its `down` pipeline

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |

## `homeboy rig repair`

```sh
homeboy rig repair <RIG_ID>
```

Repair safe declared drift without running the full `up` pipeline

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

## `homeboy rig sync`

```sh
homeboy rig sync [OPTIONS] <RIG_ID>
```

Sync every stack declared by this rig's components

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Print what WOULD happen without mutating stack specs or target branches |

## `homeboy rig run`

```sh
homeboy rig run [OPTIONS] <RIG_ID> [ARGS]...
```

Refresh, sync, check, and benchmark a rig profile end-to-end

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |
| `[ARGS]...` | no | Additional arguments to pass to the bench runner (must follow --) |

| Option | Value | Description |
| --- | --- | --- |
| `--profile` | `<PROFILE>` | Rig-defined bench profile to run |
| `--component` | `<COMPONENT>` | Optional component ID override for rigs with multiple bench components |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--iterations` | `<ITERATIONS>` | Iterations per scenario. Forwarded to the bench runner |
| `--warmup` | `<N>` | Warmup iterations to run before measured iterations |
| `--runs` | `<COUNT>` | Number of repetitions (independent substrate spawns) |
| `--run-id` | `<ID>` | Caller-supplied stable proof label for this run |
| `--shared-state` | `<DIR>` | Directory shared across bench runner instances |
| `--concurrency` | `<CONCURRENCY>` | Number of concurrent bench runner instances |
| `--settings-json-file` | `<FILE>` | Load typed setting overrides from a JSON object file. Repeatable |
| `--setting` | `<KEY=VALUE>` | String setting override. Repeatable |
| `--setting-json` | `<SETTING_JSON>` | Typed-JSON setting override. Repeatable |
| `--json-summary` | flag | Print compact machine-readable bench summary |

## `homeboy rig status`

```sh
homeboy rig status <RIG_ID>
```

Show current state of a rig: running services, last up/check

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

## `homeboy rig release-lock`

```sh
homeboy rig release-lock [OPTIONS] <RIG_ID>
```

Release a stuck active-run lock (rig lease) so a new run can proceed.

By default the lock is only released when its holder is provably gone or past its TTL. Pass `--force` to reclaim a lock whose holder is still alive but wedged. Releasing the lock frees the local guardrail; it does not terminate the holder process.

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--force` | flag | Reclaim the lock even if its holder process is still alive |

## `homeboy rig install`

```sh
homeboy rig install [OPTIONS] <SOURCE>
```

Install rigs from a local package path or git URL

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | Git URL or local path containing rig.json or rigs/<id>/rig.json |

| Option | Value | Description |
| --- | --- | --- |
| `--id` | `<ID>` | Install a specific rig from a multi-rig package |
| `--all` | flag | Install every rig in the package |
| `--reinstall` | flag | Explicitly refresh an existing matching rig install. Refuses user-owned conflicts |

## `homeboy rig update`

```sh
homeboy rig update [OPTIONS] [RIG_ID]
```

Update rigs installed from git-backed rig packages

| Argument | Required | Description |
| --- | --- | --- |
| `[RIG_ID]` | no | Rig ID to update. Updates the source package that owns this rig |

| Option | Value | Description |
| --- | --- | --- |
| `--all` | flag | Update every installed git-backed rig source package |

## `homeboy rig sources`

```sh
homeboy rig sources [COMMAND]
```

Inspect or remove installed rig sources

| Subcommand | Summary |
| --- | --- |
| `homeboy rig sources list` | List installed rig source packages |
| `homeboy rig sources remove` | Remove rigs installed from a source package |
| `homeboy rig sources refresh` | Refresh rigs installed from recorded source package paths |

## `homeboy rig sources list`

```sh
homeboy rig sources list
```

List installed rig source packages

## `homeboy rig sources remove`

```sh
homeboy rig sources remove <SOURCE>
```

Remove rigs installed from a source package

| Argument | Required | Description |
| --- | --- | --- |
| `<SOURCE>` | yes | Source URL/path, package path, or package ID from `rig sources list` |

## `homeboy rig sources refresh`

```sh
homeboy rig sources refresh [SOURCE]
```

Refresh rigs installed from recorded source package paths

| Argument | Required | Description |
| --- | --- | --- |
| `[SOURCE]` | no | Source URL/path, package path, or package ID from `rig sources list`. Omit to refresh every installed git-backed source package |

## `homeboy rig app`

```sh
homeboy rig app <COMMAND>
```

Install, update, or remove this rig's desktop app launcher

| Subcommand | Summary |
| --- | --- |
| `homeboy rig app install` | Generate and install this rig's configured launcher |
| `homeboy rig app update` | Regenerate this rig's configured launcher |
| `homeboy rig app uninstall` | Remove this rig's configured launcher |

## `homeboy rig app install`

```sh
homeboy rig app install [OPTIONS] <RIG_ID>
```

Generate and install this rig's configured launcher

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Print generated paths without writing files |

## `homeboy rig app update`

```sh
homeboy rig app update [OPTIONS] <RIG_ID>
```

Regenerate this rig's configured launcher

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Print generated paths without writing files |

## `homeboy rig app uninstall`

```sh
homeboy rig app uninstall [OPTIONS] <RIG_ID>
```

Remove this rig's configured launcher

| Argument | Required | Description |
| --- | --- | --- |
| `<RIG_ID>` | yes | Rig ID |

| Option | Value | Description |
| --- | --- | --- |
| `--dry-run` | flag | Print generated paths without deleting files |

## `homeboy rig artifact`

```sh
homeboy rig artifact <COMMAND>
```

Register local command-step evidence with the enclosing rig run

| Subcommand | Summary |
| --- | --- |
| `homeboy rig artifact register` | Register an existing local file or directory with the current rig run |

## `homeboy rig artifact register`

```sh
homeboy rig artifact register [OPTIONS]
```

Register an existing local file or directory with the current rig run

| Option | Value | Description |
| --- | --- | --- |
| `--kind` | `<KIND>` | Stable artifact kind used by run artifact readers |
| `--path` | `<PATH>` | Existing local file or directory to retain |

