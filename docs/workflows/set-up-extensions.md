# Set Up Extensions

Extensions teach Homeboy how to operate a technology stack while keeping core generic. Core owns command shape, configuration resolution, output envelopes, runners, and evidence. Extensions own ecosystem semantics such as Cargo, WP-CLI, package managers, framework-specific checks, release actions, and deploy behavior.

## Use This When

- A component needs platform-specific lint, test, build, release, deploy, fuzz, trace, or CLI behavior.
- A repo has a `homeboy.json` that names extensions not installed locally.
- A runner must have the same extension behavior as the controller.
- You are authoring a new extension and need to know which contracts matter.

## 1. Install An Extension

Install from a git source:

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust
```

Install from a local source during development:

```bash
homeboy extension install /path/to/extension --id my-extension
```

Install every extension configured by a component:

```bash
homeboy extension install-for-component --source /path/to/extensions --path /path/to/component
```

## 2. Inspect Readiness

```bash
homeboy extension list
homeboy extension show <extension-id>
homeboy extension setup <extension-id>
```

`show` reports manifest, runtime, capability, and readiness details. Use it before assuming a component's configured extension can run locally or on a runner.

## 3. Wire The Component

Portable repo config usually names the extension in `homeboy.json`:

```json
{
  "id": "my-component",
  "extensions": {
    "rust": {}
  }
}
```

Extension settings merge across project and component scopes. Component settings travel with the repo; project settings describe environment-specific behavior.

## 4. Run Through Homeboy, Not Around It

Prefer Homeboy commands over direct extension scripts:

```bash
homeboy lint my-component
homeboy test my-component
homeboy build my-component
homeboy review my-component --changed-since origin/main
```

Use `extension run`, `extension action`, or `extension exec` when you need extension-owned operator behavior directly:

```bash
homeboy extension run <extension-id> --component <component-id> -- <args>
homeboy extension action <extension-id> <action-id> --project <project-id>
homeboy extension exec <extension-id> --component <component-id> -- <command>
```

These are operator surfaces because forwarded commands may mutate targets.

## 5. Know The Core Contracts

Important extension contracts include:

- Capability scripts: `lint`, `test`, `build`, and component-owned script overrides.
- Runtime config: `run_command`, `setup_command`, `ready_check`, `env`, and entrypoints.
- Structured sidecars: declared machine-readable files emitted by extension runners.
- Deploy configuration: archive install policy, deploy overrides, verification, and hooks.
- Release actions: extension actions named for release steps.
- Fuzz workloads and trace/bench behavior when the extension supports those workflows.

Core provides the generic execution context, JSON envelope, runner/offload boundary, artifact persistence, and safety manifest. Extensions provide domain behavior.

## 6. Keep Runner Parity In Mind

If commands run through a Lab runner, the runner must have compatible Homeboy and extension behavior:

```bash
homeboy --runner <runner-id> extension show <extension-id>
homeboy --runner <runner-id> extension update <extension-id>
homeboy runner doctor <runner-id> --scope lab-offload
```

Do this before treating runner output as release-gate proof.

## Reference

- [extension command](../commands/extension.md)
- [Extension manifest schema](../reference/schemas/extension-manifest-schema.md)
- [Runner contract](../architecture/runner-contract.md)
- [Set up Lab runners](set-up-lab-runners.md)
