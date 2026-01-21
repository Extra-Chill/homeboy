# Homeboy (CLI + core) agent notes

## Architecture quick map
- CLI entry is in src/main.rs: builds clap commands, decides output mode (JSON vs raw Markdown vs interactive passthrough), and maps results to the JSON envelope in src/output/response.rs.
- CLI command handlers live in src/commands/* and return (OutputStruct, exit_code); they call into core modules in src/core/* (e.g., commands/project.rs -> core/project.rs).
- Core config entities use the generic helpers in src/core/config.rs with JSON specs (object or array) and file-backed storage under the OS config dir from src/core/paths.rs.
- Docs are embedded at build time: build.rs generates generated_docs.rs from docs/; runtime resolution is in src/docs/mod.rs. Doc topics map to docs paths without .md (e.g., docs/commands/deploy.md → commands/deploy).
- Modules are first-class: manifests live under the Homeboy config dir (modules/<id>/<id>.json or legacy homeboy.json). Core module logic is in src/core/module.rs and drives dynamic CLI subcommands in src/main.rs.

## Project-specific patterns to follow
- JSON output is mandatory for most commands; wrap results with the stable envelope in src/output/response.rs and return exit codes via map_cmd_result_to_json.
- Some commands are raw/interactive: ssh/logs passthrough require a TTY (see src/main.rs and src/tty.rs), docs/changelog/list return Markdown.
- CLI “set/merge/remove” flows accept JSON specs from a string, `@file`, or stdin (`-`); avoid inventing new parsing conventions—reuse `merge_json_sources` (CLI layer, `src/commands/mod.rs`) and the centralized reader `read_json_spec_to_string` at `src/core/config.rs`.
- Config records are stored as JSON files (projects/, servers/, components/) under the OS config dir; avoid adding repo-local config files.

## Key integration points
- Module execution context is passed via env vars in src/core/module.rs (HOMEBOY_EXEC_CONTEXT_*). Module command templates use {{modulePath}}, {{entrypoint}}, {{args}}, and project context vars.
- Docs and JSON output contracts are authoritative in docs/ (see docs/index.md and docs/json-output/json-output-contract.md).

## Workflow reminders
- Tests are expected to run in release mode: cargo test --release (see CLAUDE.md).
- For CLI validation, prefer: cargo run --release -p homeboy -- <args> (per CLAUDE.md).
