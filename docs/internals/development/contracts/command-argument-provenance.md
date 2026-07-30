# Command argument provenance contract

CLI compilation produces a typed command together with a source map for every
resolved Clap argument. The public Rust surface is:

```rust
use homeboy_cli::cli_surface::{
    ArgumentSource, CommandArgumentProvenance, CompiledCommand,
    TrackerCookArgumentAdapter,
};
```

`CompiledCommand<T>` carries the typed command and its
`CommandArgumentProvenance`. Parser sources are `command_line`, `environment`,
and `default`. Resolution layers may annotate values as `configuration`,
`policy`, or `generated` before validation or durable-plan creation.

## Durable schema

Cook plans and evidence store the source map in their existing free-form
`metadata` object:

```json
{
  "command_argument_provenance": {
    "no_finalize": "command_line",
    "base": "configuration",
    "attempt_run_id": "generated"
  }
}
```

The metadata key is optional. Readers that do not understand it continue to
receive the existing plan schema unchanged, while readers that do understand it
must preserve it when forwarding, serializing, or rehydrating a plan. This is
why split-placement Cook transfers the typed plan through `agent-task run-plan`
instead of recompiling runner-side arguments.

## Policy adapters

Policy compilers that already have typed values use
`TrackerCookArgumentAdapter::compile` to create the same public contract. The
adapter accepts the resolved value and canonical argument/source pairs, keeping
tracker-specific resolution outside the generic CLI surface. A tracker Cook
implementation such as #10889 can call `require_policy_owned` before it creates
a workspace or discovers a provider, rejecting command-line overrides of
policy-owned fields.

Cook currently requires `--no-finalize` to be sourced from `command_line` when
provenance is available. The check happens before workspace provisioning,
provider discovery, or other external effects.
