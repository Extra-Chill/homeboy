# `homeboy cargo`

Run Cargo commands through an extension-provided top-level CLI verb.

## Synopsis

```sh
homeboy cargo <project_id> [args]...
```

## Usage

`cargo` is not a core subcommand compiled into Homeboy. It is registered at
runtime by an installed extension with CLI tool metadata, typically the Rust
extension. Use this command when a component or project is configured for
Rust-aware command execution and you want Homeboy to route the Cargo invocation
through the project/extension layer.

If `homeboy cargo ...` is unavailable, install or link the extension that
provides the Cargo CLI tool. Static core docs describe the extension contract;
availability in a specific installation depends on installed extension metadata
and can be confirmed with `homeboy --help` or `homeboy docs list`.

## Related

- [extension](extension.md)
