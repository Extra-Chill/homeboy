# Fake Agent Runtime

This fixture is a minimal generic runtime package used by contract tests. It is
not an operator runtime and should not be installed for real agent tasks.

It demonstrates:

- Required manifest fields.
- `{{runtime_path}}` provider command interpolation.
- Secret names declared without values.
- Capability and workspace materialization declarations.
- A provider command that reads an AgentTaskRequest from stdin and writes a
  normalized AgentTaskOutcome to stdout.
