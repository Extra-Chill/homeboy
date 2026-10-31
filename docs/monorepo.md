# Repository layout

Homeboy, its extensions, and the desktop app live in this one repository (#16783).

| Path | Contents | Releases | CI |
|---|---|---|---|
| `crates/`, root `Cargo.toml` | The Homeboy CLI and its internal crates | `release.yml`, tagged `vX.Y.Z` | `ci.yml` (skips extension- and desktop-only changes) |
| `extensions/` | The extensions (`wordpress`, `rust`, `nodejs`, …), formerly `Extra-Chill/homeboy-extensions` | `extensions-release.yml`, tagged `<extension-id>-vX.Y.Z` | `extensions-self-checks.yml`, `extensions-wp-codebox-test-runtime-canary.yml` |
| `desktop/` | The macOS app, formerly `Extra-Chill/homeboy-desktop` | `extensions-release.yml`, tagged `homeboy-desktop-vX.Y.Z` (tag and changelog; no signed DMG yet) | `desktop.yml` (macOS build and contract tests) |

Both imports kept their full history, so `git log -- extensions/wordpress` and
`git blame` work across the move. Tags that collided with core's `vX.Y.Z` were
renamed: homeboy-extensions' own `v*` tags are now `homeboy-extensions-v*`, and
homeboy-desktop's are `homeboy-desktop-v*`. Per-extension tags kept their names.

## How the parts stay separate

- Root `homeboy.json` sets `scopes.defaults.exclude` to `extensions` and
  `desktop`, so core's release, lint, and test ignore commits under those paths.
- Root `Cargo.toml` excludes `extensions` from the workspace. The Rust
  extension's test fixtures are standalone Cargo projects.
- Extension self-checks source core's runtime helpers
  (`crates/homeboy-core/src/extension/runtime/`) from the same checkout. A
  change there runs the extension checks as well.

## Installing extensions

`homeboy extension install https://github.com/Extra-Chill/homeboy --id wordpress`
finds the extension under `extensions/` and clones only that path. The old
`Extra-Chill/homeboy-extensions` URL keeps working for existing installs.

## Reusable workflows

`runtime-agent-full-run.yml` and `actions-run-once.yml` are reusable
(`workflow_call`) workflows, so they live in the root `.github/workflows/`.
Call them as `Extra-Chill/homeboy/.github/workflows/<name>@<ref>`; see
[reusable-workflows.md](reusable-workflows.md). Pins to the old
`Extra-Chill/homeboy-extensions/.github/workflows/...` paths keep resolving from
the frozen repository.
