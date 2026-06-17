# Homeboy Extensions

Official extension directory for [Homeboy](https://github.com/Extra-Chill/homeboy). Each extension is a project-type primitive — it teaches Homeboy how to discover, audit, lint, test, and release a particular kind of codebase.

This is a **monorepo** — each subdirectory is a standalone extension. Install individual extensions, not the whole repo.

Agent runtimes live under `agent-runtimes/`. The WP Codebox runtime is exposed at
`agent-runtimes/wp-codebox` as a first-class runtime package; project extensions can
depend on runtime capabilities without embedding the provider contract.

Generic runtime package requirements are documented in
[`docs/agent-runtime-package-contract.md`](docs/agent-runtime-package-contract.md).
Use the `agent-runtimes/fake-runtime` fixture as the minimal contract example;
WP Codebox is a concrete backend, not the generic template.

Generic project-root, package-manager, and named-script execution helpers live in
[`scripts/lib/project-scripts.sh`](scripts/lib/project-scripts.sh) and are
documented in [`docs/project-script-runtime.md`](docs/project-script-runtime.md).
They are extension-owned until Homeboy core grows an ecosystem-neutral runtime
helper contract.

## Available Extensions

| Extension | Description |
|-----------|-------------|
| `wordpress` | WordPress project type — WP-CLI, build, test (via Playground), and lint (PHPCS + WPCS + PHPStan + ESLint) |
| `rust` | Rust project type — Cargo CLI, fingerprint/refactor, test/lint runners, crates.io publish |
| `nodejs` | Node.js project type — discovery, audit grammar (Express/Mongoose/etc.), and npm release lifecycle |
| `go` | Go project type — Cargo-equivalent CLI integration for services and binaries |
| `swift` | Swift project type — testing infrastructure for macOS, iOS, and Swift CLI projects |
| `managed-preview` | Provider command helpers for Homeboy managed service public previews |

## Scope

These extensions cover the **codebase lifecycle**: discover, audit, lint, test, refactor, release, deploy. Anything that runs as a long-lived process, talks to a remote API at runtime, or scrapes/automates external services belongs in [Sweatpants](https://github.com/Extra-Chill/sweatpants), not here.

GitHub Releases and Homebrew tap publishing for Rust binaries are handled by [cargo-dist](https://github.com/axodotdev/cargo-dist) directly inside Homeboy's own release workflow — there is no separate `github` or `homebrew` extension.

## Installation

Install extensions one at a time. Homeboy clones this repo, detects the monorepo layout, and extracts just the extension you asked for into `~/.config/homeboy/extensions/<id>/`.

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id wordpress
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id rust
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id nodejs
```

Remote runners need the same extension IDs installed on the runner host before
Homeboy can offload matching jobs to them. For the standard runner bootstrap
path, see [`docs/remote-runner-bootstrap.md`](docs/remote-runner-bootstrap.md).

### Verify installation

```bash
# List all installed extensions
homeboy extension list

# Inspect a specific extension
homeboy extension show wordpress
```

### Install from a local clone

Local path installs are for active extension development:

```bash
git clone https://github.com/Extra-Chill/homeboy-extensions.git
homeboy extension install ./homeboy-extensions/wordpress
homeboy extension install ./homeboy-extensions/rust
```

### Install modes and updates

For normal use, install from the GitHub monorepo URL with `--id <extension>`. Homeboy manages the cloned/extracted install and can update that installed copy:

```bash
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id wordpress
```

Local path installs are linked installs. The active extension code is whatever the installed symlink target points at, so updating the primary `homeboy-extensions` checkout does not update an extension linked to another checkout or feature worktree. Avoid linking installed extensions to short-lived worktrees unless you are intentionally testing that branch.

Inspect the current state before debugging extension behavior:

```bash
homeboy extension list
homeboy extension show wordpress
readlink ~/.config/homeboy/extensions/wordpress
```

To reset a stale linked install, uninstall and reinstall from the GitHub URL, or relink intentionally to the checkout you want active.

## Usage

Once installed, declare the extension in your repo's `homeboy.json` and Homeboy will pick it up automatically:

```json
{
  "id": "my-project",
  "extensions": {
    "wordpress": {}
  }
}
```

Then run Homeboy commands as usual:

```bash
homeboy audit
homeboy lint --fix
homeboy test
homeboy release
```

### Data Machine Code promotion provider

Homeboy core can promote agent-task patch artifacts through an external
workspace provider command. This repo provides a Data Machine Code-backed
provider script for environments that use managed DMC worktrees:

```bash
homeboy agent-task promote aggregate.json \
  --to-worktree repo@branch-slug \
  --provider-command ./scripts/datamachine-code-promotion-provider.sh
```

The provider reads `homeboy/agent-task-promotion-apply-request/v1` JSON on
stdin and writes `homeboy/agent-task-promotion-apply-response/v1` JSON on
stdout. It keeps the DMC-specific `studio wp datamachine-code ...` shellouts in
Homeboy Extensions instead of Homeboy core.

### Data Machine Agent CI runtime tasks

The reusable `.github/workflows/datamachine-agent-ci.yml` workflow can run a
generic WordPress ability directly through WP Codebox, without importing an
agent bundle. Use `execution_kind: runtime_task` with either `runtime_task` or
the `ability_request`/`ability_input` shorthand. Downloaded GitHub Actions
artifacts can be mounted into the sandbox with `runtime_mounts`, and typed
outputs can be enforced with `artifact_declarations` plus `output_mappings`:

```yaml
jobs:
  process-artifact:
    uses: Extra-Chill/homeboy-extensions/.github/workflows/datamachine-agent-ci.yml@main
    with:
      homeboy_extensions_ref: main
      target_repo: Example/project
      agent_slug: artifact-processor
      pipeline_slug: artifact-pipeline
      flow_slug: artifact-flow
      execution_kind: runtime_task
      actions_artifact_downloads: >-
        [{"repo":"Example/project","run_id":"123456789","name":"source-packet","dir":".ci/actions-artifacts/source-packet"}]
      runtime_mounts: >-
        [{"source":".ci/actions-artifacts/source-packet","target":"/workspace/input","mode":"readonly"}]
      ability_request: >-
        {"ability":"example/process-artifact","input":{"source_artifact":"/workspace/input/source.json"}}
      output_mappings: >-
        {"processed_packet":"result.processed_packet"}
      artifact_declarations: >-
        [{"name":"processed_packet","type":"example-packet","schema":"example/processed-packet/v1","required":true}]
      expected_artifacts: '["processed_packet"]'
```

Use `component_contracts` only when the ability provider plugin or runtime
component must be mounted explicitly. Keep ability names, schemas, and artifact
types owned by the caller; Homeboy Extensions only forwards the generic runtime
task contract.

Each extension also exposes a CLI binding for direct use against a project or component:

```bash
# WordPress
homeboy wp my-site plugin list

# Rust
homeboy cargo my-crate build
```

## Creating Extensions

Each extension is a directory containing a `<extension-id>.json` manifest. The manifest defines capabilities, audit grammar, release actions, and CLI bindings. See `wordpress/wordpress.json` and `rust/rust.json` for full examples.

Each extension owns its own `CHANGELOG.md` at `<extension>/docs/CHANGELOG.md`, generated by Homeboy at release time. There is no monorepo-level changelog — extensions release independently.
