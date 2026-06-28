# Architecture Cleanup Map

This map tracks lower-risk source-boundary cleanup. It is intentionally scoped to
developer guidance and ratchets; command implementation refactors should happen
in focused command-cleanup PRs.

## Core And Command Boundary

Homeboy keeps the CLI layer thin:

- `src/cli_surface.rs` owns clap command shape, hidden compatibility aliases, and command-surface introspection.
- `src/command_contract/` owns command registry metadata, output families, and Lab portability contracts.
- `src/commands/` maps parsed arguments to responses and delegates durable behavior.
- `src/core/` owns reusable services, persistence, runner dispatch, artifact lifecycles, release/test/audit workflows, and extension contracts.

New command work should introduce or reuse a `core` service boundary before it
adds orchestration. Command modules may contain small glue, validation, and
response mapping. They should not become the owner of direct process execution,
filesystem mutation, run artifact persistence, or runner dispatch.

## Current Thin-Command Cleanup Map

`homeboy.json` already enables `audit.thin_command_adapter` for `src/commands/`.
The configured orchestration markers are:

- Direct process execution: `std::process::Command`, `Command::new(...)`.
- Direct filesystem mutation: `std::fs` or `fs` writes, directory creation, removal, rename, or copy.
- Direct file writer construction: `File::create(...)`, `OpenOptions::new(...)`.
- Run artifact persistence: `RunDir::create`, `write_to_run_dir`, `ObservationBuilder`, `finish_run`, and `persist_*` helpers.
- Runner execution orchestration: `runner::exec`, `execute_*`, and `dispatch_*` calls.

The policy allows a small amount of adapter glue (`max_orchestration_weight: 3`)
and supports explicit allowlist comments for known intentional adapters. Cleanup
should retire existing findings by moving behavior behind core services, then
ratchet the audit baseline. New command modules should not add new findings.

Recommended service boundaries:

- Process execution: define an operation-specific `core` function or facade that receives typed inputs and returns typed output.
- Filesystem mutation: keep path normalization and mutation in `core`, with commands passing requested intent.
- Runner dispatch: route through `core::runners` facades rather than command-owned dispatch loops.
- Artifact persistence: persist through `core::artifacts` or operation-specific core result writers.

## Static Guards

Existing tests already protect several boundaries:

- `tests/architecture_core_agnostic_test.rs::core_source_does_not_depend_on_command_layer` prevents `src/core` from importing `commands` or CLI surface modules.
- `tests/architecture_core_agnostic_test.rs::command_layer_uses_explicit_core_facades_only` keeps command imports on explicit core facades instead of private implementation modules.
- `tests/architecture_core_agnostic_test.rs::core_facades_expose_explicit_groups_not_wildcards` prevents facade wildcard exports from hiding accidental public surface growth.
- `tests/architecture_core_agnostic_test.rs::architecture_docs_source_paths_exist` verifies architecture/developer-guide markdown claims that reference `src/...` paths.
- `homeboy.json` `audit.thin_command_adapter` is the ratchet for command-layer orchestration density.

Run lightweight verification through the normal test suite in CI. Local agents on
resource-constrained machines can still review these guards as static source
checks without running Cargo locally.

## Compatibility-Removal Inventory

These entries are intentionally left as inventory rather than removed in this
docs cleanup PR.

| Compatibility surface | Current owner | Current shape | Retirement criteria |
| --- | --- | --- | --- |
| Hidden `homeboy list` command | `src/cli_surface.rs`, `src/command_contract/output.rs` | Hidden raw Markdown/help alias with optional JSON safety manifest. | Remove after command-surface manifest consumers use `homeboy --help`, `homeboy docs list`, or the explicit safety manifest surface. |
| Global `--no-local-execution` alias | `src/cli_surface.rs` | Visible alias for `--lab-only`. | Remove after operator docs and automation uniformly use `--lab-only` and no active scripts reference the alias. |
| Legacy component fields such as `build_command` | `src/commands/component.rs`, `src/core/extension/build/mod.rs`, `src/core/extension/capability.rs` | Rejected with targeted errors while modern config uses extension/build script contracts. | Remove parse-time compatibility handling after persisted configs have been migrated and error telemetry shows the legacy field is no longer encountered. |
| Hidden JSON self-check flags | `src/commands/lint.rs`, `src/commands/test.rs`, `src/commands/review/mod.rs` | Hidden `--self-checks-json`-style command inputs used by internal checks. | Replace with explicit core/test harness contracts, then remove hidden flags once self-check callers are migrated. |
| Legacy CLI aliases rejected by argument normalization | `src/commands/utils/args.rs` | Rejection tests protect known old aliases from silently routing. | Keep rejection coverage until the aliases are old enough to delete from compatibility messaging. |
| Legacy `node_script` extension fields | Extension/runtime manifests and config migrations, if still observed in downstream configs. | Historical extension runtime field name; modern manifests should use runtime command/script contracts. | Inventory live configs, migrate any remaining manifests, then add a rejection or schema cleanup PR. |
| Old deleted command names | Command docs, shell completions, user scripts. | Some removed commands may still appear in docs or automation outside the command surface. | Run a command-surface/docs audit, update references to supported commands, then remove compatibility notes. |

## Tracking Rules

- Prefer one cleanup PR per command family or service boundary.
- Update this map when a compatibility surface is retired or when a new boundary guard lands.
- Ratchet `homeboy.json` baselines only after the underlying code cleanup is merged.
- Keep extension- or ecosystem-specific behavior in extension manifests, scripts, hooks, docs, and runtime packages instead of Homeboy core.
