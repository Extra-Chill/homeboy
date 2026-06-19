# Fake Agent Runtime

This fixture is a minimal generic runtime package used by contract tests. It is
not an operator runtime and should not be installed for real agent tasks.

It demonstrates:

- Required manifest fields.
- `{{runtime_path}}` provider command interpolation.
- Secret names declared without values.
- Capability and workspace materialization declarations.
- Runtime-neutral `tool_presets` expansion for runner workspace and publication
  tools.
- A provider command that reads an AgentTaskRequest from stdin and writes a
  normalized AgentTaskOutcome to stdout.

## Tool Presets

Generic runtime manifests can advertise `tool_presets` instead of copying a
product-specific tool list into every provider. The shared contract currently
defines:

- `runner_workspace`: read-only workspace inspection plus read-write command,
  file edit, patch, delete, and git-add tool ids.
- `publication`: preparation, publish, and status tool ids for runtimes that can
  expose reviewer-facing output.

The expanded fields are plain `workspace_tools` and `publication_tools` entries.
They intentionally avoid product-specific names so shell, OpenCode, Codex,
Claude, or other harnesses can map the ids to their own local implementation.
