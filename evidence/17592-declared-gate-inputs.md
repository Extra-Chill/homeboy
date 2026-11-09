# Issue #17592: declared gate inputs

## Changes

- Node.js 3.9.8 declares `playwright-browsers`, mapping `PLAYWRIGHT_BROWSERS_PATH` to host `HOME/.cache/ms-playwright`.
- The mapping format cannot express per-platform defaults. This is explicitly the Linux default, not the macOS or Windows location; the browser cache must already exist.
- Repeatable `--gate-file '{"id":"accept-gate","env":"ACCEPT_GATE","source":"/absolute/accept-gate.sh"}'` supplies both the gate environment path and read-only provider evidence. The workflow `cook` key comes automatically from the live command tree.
- `--gate-extension-input` copies extension directories and `--gate-input-source` supplies verification provenance; neither replaces a shared gate/provider file declaration.
- Command documentation includes the one-flag example and explains host-file versus immutable provider-projection semantics.
- Regression coverage executes a gate requesting the shipped Node.js mapping, covers gate-file expansion and forwarding, and rejects invalid declarations before dispatch.

## Validation

- `cargo test -p homeboy-agents --lib agent_task_gate::tests::declared_playwright_browsers_resolves_nodejs_extension_mapping`: 1 passed.
- `cargo test -p homeboy-cli --lib commands::agent_task::args::definitions::cook::tests`: 26 passed.
- All 34 `extensions/nodejs/homeboy.json` test commands: passed. In this flattened checkout, configured `HOMEBOY_RUNTIME_*` shell helper paths to `crates/homeboy-core/src/extension/runtime/`, `HOMEBOY_RUNTIME_SETTINGS_HELPER` to its `settings.sh`, and `HOMEBOY_RUNTIME_EXECUTABLE` to `/home/chubes/.local/bin/homeboy`. Initial runs without those declarations failed helper discovery; the configured run passed all checks.
- `git diff --check`: passed.

## PR body

Finish #17592 with the Node.js Playwright browser-cache declaration and one shared gate-file input for gates and provider evidence. Document the new flag and automatically derived workflow key, and add regression coverage for extension resolution, file expansion, forwarding, and admission validation.

The extension mapping format has no per-platform path support, so the Playwright mapping uses the Linux default `HOME/.cache/ms-playwright`, not macOS or Windows defaults. Bump Node.js to 3.9.8 and record the change in its changelog.

Validation: Playwright gate regression (1 passed), Cook argument module tests (26 passed), and all 34 Node.js extension self-check tests passed.

Closes #17592.

Implemented by Homeboy Cook (Pi, openai/gpt-6.1-sol). Issue, prompt and review by Franklin (Claude Opus via Pi, Roadie).
