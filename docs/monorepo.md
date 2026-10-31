# Repository layout

Homeboy, its extensions, and the desktop app live in this one repository (#16783).

| Path | Contents | Releases | CI |
|---|---|---|---|
| `crates/`, root `Cargo.toml` | The Homeboy CLI and its internal crates | `release.yml`, tagged `vX.Y.Z` | `ci.yml` (skips extension- and desktop-only changes) |
| `extensions/` | The extensions (`wordpress`, `rust`, `nodejs`, …), formerly `Extra-Chill/homeboy-extensions` | `extensions-release.yml`, tagged `<extension-id>-vX.Y.Z` | `extensions-self-checks.yml`, `extensions-wp-codebox-test-runtime-canary.yml` |
| `desktop/` | The macOS app, formerly `Extra-Chill/homeboy-desktop` | none yet (it was released by hand) | none yet |

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

## Not moved yet

`extensions/.github/workflows/` still holds `runtime-agent-full-run.yml` and
`actions-run-once.yml`. Those are reusable workflows (`workflow_call`), and
GitHub only runs them from a repository's root `.github/workflows/`. Callers
pin them in `Extra-Chill/homeboy-extensions` by SHA, and those pins keep
resolving after that repository is frozen. They move to the root at cutover.
