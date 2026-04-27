# Homeboy CLI Parity Matrix

This matrix maps the current `homeboy` CLI command surface to Homeboy Desktop support.

Source of truth checked for this pass: `homeboy --help` and top-level `homeboy <command> --help` from the installed CLI on 2026-04-27.

## Status Legend

- **supported**: the desktop app exposes the command family with current-enough wrappers for normal use.
- **partial/stale**: the desktop app exposes part of the command family, but command shapes or models are known stale.
- **missing**: no meaningful desktop surface exists.
- **intentionally CLI-only**: the command is best kept in terminal workflows for now.

## Matrix

| CLI command | Desktop support status | Current app surface / file references | Recommended UI/API work | Tracking issue |
|---|---|---|---|---|
| `homeboy project` | partial/stale | Settings project fields and project switcher; `Homeboy/Core/CLI/HomeboyCLI.swift` project wrappers; stale `CLIBridge` `--json` wrappers in `Homeboy/Core/CLI/CLIBridge.swift`; pin calls in `Homeboy/Extensions/RemoteFileEditor/RemoteFileEditorViewModel.swift` and `Homeboy/Extensions/RemoteLogViewer/RemoteLogViewerViewModel.swift`. | Modernize stale read/write wrappers, replace deprecated workspace discovery, and keep project pin add/remove aligned with the current `project pin` contract. | Existing: [#14](https://github.com/Extra-Chill/homeboy-desktop/issues/14), [#16](https://github.com/Extra-Chill/homeboy-desktop/issues/16), [#20](https://github.com/Extra-Chill/homeboy-desktop/issues/20), [#24](https://github.com/Extra-Chill/homeboy-desktop/issues/24) |
| `homeboy ssh` | partial/stale | Generic SSH command helper in `Homeboy/Core/CLI/HomeboyCLI.swift`; server setup surfaces in `Homeboy/Views/Settings/ServersSettingsTab.swift`. | Keep as supporting infrastructure for file/log/db/deploy views. Add a terminal-like SSH command runner only if users need ad-hoc command execution beyond existing tools. | Covered by matrix; no new issue. |
| `homeboy server` | partial/stale | Server settings UI in `Homeboy/Views/Settings/ServersSettingsTab.swift`; key helpers and CRUD wrappers in `Homeboy/Core/CLI/HomeboyCLI.swift`; stale `CLIBridge` `--json` wrappers. | Modernize read wrappers, confirm `server key` subcommands still match, and keep SSH-key status as a shared service for remote tools. | Existing: [#20](https://github.com/Extra-Chill/homeboy-desktop/issues/20) |
| `homeboy test` | missing | No dedicated test runner UI. | Add through the proposed Quality workspace rather than a standalone sidebar item. Support changed-since/changed-only scope, coverage summary, and failure output. | New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy bench` | missing | No benchmark wrappers or UI. | Add a Benchmark workspace with scenario listing, run configuration, result summaries, baseline/ratchet confirmation, and later rig comparisons. | New: [#34](https://github.com/Extra-Chill/homeboy-desktop/issues/34) |
| `homeboy lint` | missing | No direct lint wrappers; old audit/refactor wrappers do not cover modern lint. | Add through the Quality workspace. Treat `lint --fix` as a confirmed write action. | New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy db` | partial/stale | Database browser in `Homeboy/Extensions/DatabaseBrowser/`; wrappers in `Homeboy/Core/CLI/HomeboyCLI.swift` for `tables`, `describe`, `query`, `search`, `delete-row`, `drop-table`. | Modernize stale `db search` helper shape and verify destructive confirmation flags against current CLI. Keep database browsing as a core tool. | Existing: [#22](https://github.com/Extra-Chill/homeboy-desktop/issues/22) |
| `homeboy file` | partial/stale | File editor in `Homeboy/Extensions/RemoteFileEditor/`; wrappers in `Homeboy/Core/CLI/HomeboyCLI.swift` for list/read/write/delete/rename/find/grep. | Modernize stale `file find` and `file grep` shapes. Consider adding download/edit operations if the file editor needs richer remote workflows. | Existing: [#22](https://github.com/Extra-Chill/homeboy-desktop/issues/22) |
| `homeboy fleet` | partial/stale | Fleet wrappers in `Homeboy/Core/CLI/HomeboyCLI.swift`; limited configuration relationship through projects/components. | Modernize `fleet create` (`--projects`) and decide whether fleet status/check/exec belong in a fleet dashboard or the Quality/Deploy views. | Existing: [#21](https://github.com/Extra-Chill/homeboy-desktop/issues/21) |
| `homeboy logs` | partial/stale | Log viewer in `Homeboy/Extensions/RemoteLogViewer/`; wrappers in `Homeboy/Core/CLI/HomeboyCLI.swift` for list/show/clear/search. | Modernize stale `logs show` and `logs search` helper shapes; keep pin creation aligned with `project pin`. | Existing: [#22](https://github.com/Extra-Chill/homeboy-desktop/issues/22), [#24](https://github.com/Extra-Chill/homeboy-desktop/issues/24) |
| `homeboy transfer` | missing | No transfer workflow. | Do not force into File Editor. If needed, add a transfer sheet that models local/server source and destination plus dry-run output. | Covered by matrix; no new issue until user demand. |
| `homeboy triage` | missing | No attention-report UI. | Add through the Quality workspace as a read-only dashboard for components, projects, fleets, rigs, and workspace scope. | New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy deploy` | partial/stale | Deployer core tool in `Homeboy/Extensions/Deployer/`; direct CLI calls in `Homeboy/Extensions/Deployer/DeployerViewModel.swift`. | Keep deploy as a core tool, but split release/build/change planning into a separate workflow so deploy stays deploy-only. Verify current deploy result envelope. | New: [#36](https://github.com/Extra-Chill/homeboy-desktop/issues/36) |
| `homeboy component` | partial/stale | Components settings UI in `Homeboy/Views/Settings/ComponentsSettingsTab.swift`; wrappers/models in `Homeboy/Core/CLI/HomeboyCLI.swift`; config structs in `Homeboy/Core/Config/ComponentConfiguration.swift`. | Modernize component create/set/read shapes and add missing component model fields before building new component workflows on top. | Existing: [#15](https://github.com/Extra-Chill/homeboy-desktop/issues/15), [#20](https://github.com/Extra-Chill/homeboy-desktop/issues/20), [#21](https://github.com/Extra-Chill/homeboy-desktop/issues/21) |
| `homeboy config` | intentionally CLI-only | No global config editor beyond app settings and CLI path/version display. | Keep global config editing in CLI for now. Desktop should edit project/server/component records through focused screens, not expose raw JSON pointer mutation. | Covered by matrix; no new issue. |
| `homeboy extension` | partial/stale | Extension manager in `Homeboy/Core/Extensions/ExtensionManager.swift`; extension views under `Homeboy/Views/Extensions/`; platform action calls in `Homeboy/Views/Extensions/ExtensionContainerView.swift`. | Modernize stale list/setup/uninstall/run argument shapes. Preserve dynamic extension rendering after the command contract is current. | Existing: [#23](https://github.com/Extra-Chill/homeboy-desktop/issues/23) |
| `homeboy status` | partial/stale | Workspace discovery still calls removed/deprecated `init`; status is not a first-class desktop dashboard. | Replace `init` with `status --full`, then consider a status summary card in the main project surface. | Existing: [#14](https://github.com/Extra-Chill/homeboy-desktop/issues/14) |
| `homeboy docs` | partial/stale | Docs references in `docs/CLI.md` and `docs/EXTENSION-SPEC.md`; no in-app docs browser. | Fix stale doc references first. An in-app docs browser is optional; command docs can stay CLI-only unless users need contextual help panes. | Existing: [#26](https://github.com/Extra-Chill/homeboy-desktop/issues/26) |
| `homeboy changelog` | intentionally CLI-only | No changelog UI. | Keep generated changelog inspection in CLI. Desktop release workflow can link to changes/release output instead of editing changelogs. | Covered by matrix; no new issue. |
| `homeboy git` | missing | No Git workspace. Component state is indirectly displayed in deploy/status contexts only. | Add read-only git status/changes first, then guarded write actions only after confirmation UX exists. | New: [#37](https://github.com/Extra-Chill/homeboy-desktop/issues/37) |
| `homeboy issues` | missing | No issue reconciliation UI. | Decide whether `issues reconcile` belongs in the Quality workspace or Git/GitHub workspace; start by linking findings to issue URLs. | New: [#37](https://github.com/Extra-Chill/homeboy-desktop/issues/37) |
| `homeboy version` | missing | Version values are inferred in deploy output, but no version command surface exists. | Add to the Release/Build workflow as read-only version target inspection before any bump/release action. | New: [#36](https://github.com/Extra-Chill/homeboy-desktop/issues/36) |
| `homeboy build` | partial/stale | Deployer has local shell `buildCommand` execution in `Homeboy/Extensions/Deployer/DeployerViewModel.swift`, not `homeboy build`. | Move build execution/status toward `homeboy build` in the Release/Build workflow; keep arbitrary shell build commands only where Homeboy config requires them. | New: [#36](https://github.com/Extra-Chill/homeboy-desktop/issues/36) |
| `homeboy validate` | missing | No validation workflow. | Add through the Quality workspace as a lightweight parse/compile check. | New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy changes` | missing | No changes-since-tag UI. | Add to the Release/Build workflow as the first read-only release planning surface. | New: [#36](https://github.com/Extra-Chill/homeboy-desktop/issues/36) |
| `homeboy release` | missing | No release planning/execution UI. | Add dry-run preview first, then guarded release execution. Do not merge into Deployer; release and deploy have different risk profiles. | New: [#36](https://github.com/Extra-Chill/homeboy-desktop/issues/36) |
| `homeboy review` | missing | No review umbrella UI. | Add through the Quality workspace as the primary modern quality entry point. | New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy audit` | partial/stale | Stale quick actions in `Homeboy/Core/CLI/HomeboyCLI.swift` still call removed `audit code`, `audit docs`, and `audit structure`; no modern audit results UI. | Replace with current `homeboy audit <component>` plus filters, then surface through the Quality workspace. | Existing: [#18](https://github.com/Extra-Chill/homeboy-desktop/issues/18), [#27](https://github.com/Extra-Chill/homeboy-desktop/issues/27); New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy refactor` | partial/stale | Refactor wrappers exist in `Homeboy/Core/CLI/HomeboyCLI.swift`, but no visible workflow and at least `rename` shape is stale. | Fix stale wrappers, then decide whether refactor proposals/actions live in Quality as confirmed write operations. | Existing: [#22](https://github.com/Extra-Chill/homeboy-desktop/issues/22); New: [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) |
| `homeboy rig` | missing | No rig models, wrappers, or screens. | Add a dedicated Rigs screen; do not force rigs into Settings or Deployer. | New: [#32](https://github.com/Extra-Chill/homeboy-desktop/issues/32) |
| `homeboy stack` | missing | No stack models, wrappers, or screens. | Add a Stack Manager screen for spec listing/status first, then guarded apply/sync. | New: [#33](https://github.com/Extra-Chill/homeboy-desktop/issues/33) |
| `homeboy undo` | partial/stale | Wrappers in `Homeboy/Core/CLI/HomeboyCLI.swift` for latest/specific undo and list. No obvious visible screen. | Surface undo from workflows that perform Homeboy writes (`refactor`, `lint --fix`, release actions) rather than as a standalone global screen. | Covered by [#35](https://github.com/Extra-Chill/homeboy-desktop/issues/35) for write-capable quality actions. |
| `homeboy auth` | missing | Desktop has its own `AuthManager`, but no Homeboy project API auth wrappers. | Add a separate API/Auth workspace so Homeboy project credentials are not conflated with desktop login. | New: [#38](https://github.com/Extra-Chill/homeboy-desktop/issues/38) |
| `homeboy api` | missing | No API request explorer. | Start with authenticated GET requests and response copy/export; gate mutating verbs behind preview/confirmation. | New: [#38](https://github.com/Extra-Chill/homeboy-desktop/issues/38) |
| `homeboy upgrade` | partial/stale | CLI update UI exists in `Homeboy/Views/Settings/GeneralSettingsTab.swift`, but shells out to `brew upgrade homeboy`; app launch migration also uses brew in `Homeboy/App/HomeboyApp.swift`. | Replace brew-specific upgrade paths with `homeboy upgrade --check` / `homeboy upgrade` so source/cargo/binary installs work. | Covered by matrix; file implementation issue when stale-command burners finish. |
| `homeboy cargo` | intentionally CLI-only | No cargo wrapper. | Keep as CLI-only extension convenience. Desktop should surface component-level build/test/validate instead. | Covered by matrix; no new issue. |
| `homeboy wp` | intentionally CLI-only | WordPress settings/auth surfaces exist, but no WP-CLI passthrough. | Keep as CLI-only unless a future WordPress operations console is explicitly designed. | Covered by matrix; no new issue. |

## Follow-Up Issues Filed From This Matrix

- [#32 Add desktop rig management workflow](https://github.com/Extra-Chill/homeboy-desktop/issues/32)
- [#33 Add desktop stack manager](https://github.com/Extra-Chill/homeboy-desktop/issues/33)
- [#34 Add benchmark workspace](https://github.com/Extra-Chill/homeboy-desktop/issues/34)
- [#35 Add component quality workspace](https://github.com/Extra-Chill/homeboy-desktop/issues/35)
- [#36 Add release and build workflow](https://github.com/Extra-Chill/homeboy-desktop/issues/36)
- [#37 Add Git and GitHub operations workspace](https://github.com/Extra-Chill/homeboy-desktop/issues/37)
- [#38 Add API and auth workspace](https://github.com/Extra-Chill/homeboy-desktop/issues/38)

## Existing Stale-Command / Model Issues Referenced

- [#14 Migrate from deprecated `init` command to `status --full`](https://github.com/Extra-Chill/homeboy-desktop/issues/14)
- [#15 Update `ComponentConfiguration` with missing CLI fields](https://github.com/Extra-Chill/homeboy-desktop/issues/15)
- [#16 Update `ProjectConfiguration` with missing CLI fields](https://github.com/Extra-Chill/homeboy-desktop/issues/16)
- [#17 Track portable Homeboy component config](https://github.com/Extra-Chill/homeboy-desktop/issues/17)
- [#18 Modernize audit quick actions](https://github.com/Extra-Chill/homeboy-desktop/issues/18)
- [#19 Replace removed supports capability probe](https://github.com/Extra-Chill/homeboy-desktop/issues/19)
- [#20 Update read-only CLI calls for current JSON output contract](https://github.com/Extra-Chill/homeboy-desktop/issues/20)
- [#21 Modernize component and fleet mutation commands](https://github.com/Extra-Chill/homeboy-desktop/issues/21)
- [#22 Modernize CLI helper command shapes](https://github.com/Extra-Chill/homeboy-desktop/issues/22)
- [#23 Modernize ExtensionManager command shapes](https://github.com/Extra-Chill/homeboy-desktop/issues/23)
- [#24 Update Remote Log Viewer pin command](https://github.com/Extra-Chill/homeboy-desktop/issues/24)
- [#25 Split HomeboyCLI god file](https://github.com/Extra-Chill/homeboy-desktop/issues/25)
- [#26 Fix stale desktop documentation references](https://github.com/Extra-Chill/homeboy-desktop/issues/26)
- [#27 Improve Swift audit fingerprint coverage](https://github.com/Extra-Chill/homeboy-desktop/issues/27)
