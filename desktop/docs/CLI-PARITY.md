# Homeboy CLI Parity Matrix

This matrix maps the current `homeboy` CLI command surface to Homeboy Desktop support.

Source of truth checked for this pass: `homeboy --help` plus focused help for the workspace commands from the installed CLI on 2026-04-27, and the current desktop `main` branch.

## Status Legend

- **supported first-pass UI**: the desktop app exposes the workflow with current CLI wrappers, but may intentionally omit advanced flags.
- **read-only UI**: the desktop app exposes inspection/status output only; mutating commands stay in the CLI.
- **partial/stale**: a desktop surface exists, but some wrappers or models still need cleanup.
- **intentionally CLI-only**: the command is terminal/operator-shaped and should not get desktop UI without a stronger product need.
- **missing workflow**: no meaningful desktop surface exists yet.

## Matrix

| CLI command | Desktop support status | Current app surface / file references | Desktop boundary / remaining work | Tracking |
|---|---|---|---|---|
| `homeboy project` | supported first-pass UI | Project switcher and settings-backed project CRUD; wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Usable as app infrastructure. Raw global config editing stays CLI-only. | Matrix only |
| `homeboy ssh` | partial/stale | Generic SSH support behind file, log, db, and deploy workflows; server settings in `Homeboy/Views/Settings/ServersSettingsTab.swift`. | Keep as supporting infrastructure. Add an ad-hoc SSH console only if a real desktop workflow needs it. | Matrix only |
| `homeboy server` | supported first-pass UI | Server settings UI plus server wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Usable for normal server setup. Advanced SSH/key diagnostics stay CLI-only until a focused UI needs them. | Matrix only |
| `homeboy test` | supported first-pass UI | Quality workspace stage button via `Homeboy/Extensions/Quality/Views/QualityView.swift` and `Homeboy/Core/CLI/HomeboyCLI+QualityCommands.swift`. | Supports normal scoped runs (`changed-since`, `changed-only`, path override). Advanced analysis/drift/write flows remain CLI-only. | Matrix only |
| `homeboy bench` | supported first-pass UI | Bench workspace in `Homeboy/Views/BenchView.swift`; wrappers in `Homeboy/Core/CLI/HomeboyCLI+RigBenchReleaseCommands.swift`. | Lists scenarios and runs benchmarks with iteration control. Baseline, ratchet, multi-run, concurrency, settings, and `--rig` comparisons remain CLI-only. | Matrix only |
| `homeboy lint` | supported first-pass UI | Quality workspace stage button. | Read-only lint output is supported. `--fix`, baseline, ratchet, sniffs/category filters, and per-file/glob targeting remain CLI-only. | Matrix only |
| `homeboy db` | supported first-pass UI | Database browser in `Homeboy/Extensions/DatabaseBrowser/`; db wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Core browsing/querying works. Keep destructive operations guarded. | Matrix only |
| `homeboy file` | supported first-pass UI | Remote file editor in `Homeboy/Extensions/RemoteFileEditor/`; file wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Core list/read/write/delete/rename/search workflows exist. Keep destructive operations guarded. | Matrix only |
| `homeboy fleet` | partial/stale | Fleet relationships in settings/config models and `FleetManagementView`. | Basic configuration exists; fleet status/check/exec style operations are still CLI-shaped unless a dashboard is designed. | Matrix only |
| `homeboy logs` | supported first-pass UI | Remote log viewer in `Homeboy/Extensions/RemoteLogViewer/`; log wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Core log viewing/search exists. Pinning remains scoped to log/file workflows. | Matrix only |
| `homeboy transfer` | missing workflow | No dedicated transfer workflow. | Keep out of File Editor for now. If needed, add a focused local/server transfer sheet with dry-run output. | Matrix only |
| `homeboy triage` | supported first-pass UI | Quality workspace `Run Triage` action; `qualityTriage()` wrapper. | `triage component` is surfaced. Project/fleet/rig/workspace triage plus filters (`--mine`, labels, failing checks, drilldown) remain CLI-only. | Matrix only |
| `homeboy deploy` | supported first-pass UI | Deployer tool in `Homeboy/Extensions/Deployer/`. | Deploy remains its own guarded write workflow. Release/build planning is now separate from deployment. | Matrix only |
| `homeboy component` | supported first-pass UI | Component settings UI and models; wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Usable for normal component config. Bulk/JSON-pointer style edits stay CLI-only. | Matrix only |
| `homeboy config` | intentionally CLI-only | App settings expose focused project/server/component fields, not raw global config mutation. | Keep raw global config editing in the CLI. | Matrix only |
| `homeboy daemon` | intentionally CLI-only | No daemon management UI. | Desktop consumes CLI/API behavior; it should not manage the local daemon lifecycle unless a future diagnostic screen needs it. | Matrix only |
| `homeboy extension` | supported first-pass UI | Extension manager and dynamic extension views under `Homeboy/Core/Extensions/` and `Homeboy/Views/Extensions/`. | Extension install/setup/run exists as app infrastructure. Advanced extension authoring stays CLI/docs-driven. | Matrix only |
| `homeboy status` | supported first-pass UI | Workspace discovery now uses `status --full` in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Primary app bootstrap path is current. A richer top-level status dashboard can wait for product need. | Matrix only |
| `homeboy docs` | intentionally CLI-only | Static docs live under `docs/`; no in-app docs browser. | Keep command docs in CLI/browser docs. Contextual help panes can be added later without wrapping `homeboy docs`. | Matrix only |
| `homeboy changelog` | intentionally CLI-only | No changelog UI. | Changelog generation/inspection stays CLI-only. Release planning can link to `changes`/`release --dry-run` output instead. | Matrix only |
| `homeboy git` | read-only UI | Git workspace in `Homeboy/Extensions/GitOperations/`; `gitStatus()` wrapper plus GitHub issue/PR navigation. | Read-only `git status` is supported. `commit`, `push`, `pull`, `rebase`, `cherry-pick`, `tag`, `issue`, and `pr` writes stay CLI-only until confirmation UX exists. | Matrix only |
| `homeboy issues` | missing workflow | No issue reconciliation UI. | Could belong in Quality or Git/GitHub later. No active desktop tracker until the workflow is designed. | Matrix only |
| `homeboy version` | supported first-pass UI | Release workspace reads `version show` in `ReleaseWorkflowViewModel`. | Read-only version inspection is supported. `version bump` aliases release execution and remains CLI-only. | Matrix only |
| `homeboy build` | supported first-pass UI | Release workspace `Build` action calls `homeboy build`; deployer still has deployment-specific build handling. | Component build is supported from release planning. Bulk/project build modes remain CLI-only. | Matrix only |
| `homeboy validate` | supported first-pass UI | Quality workspace validation stage. | Direct non-mutating validation is supported. No advanced workflow beyond component/path selection. | Matrix only |
| `homeboy changes` | supported first-pass UI | Release workspace reads `homeboy changes` and renders commits since baseline. | Single-component planning is supported. Project/bulk change reports and `--git-diffs` remain CLI-only. | Matrix only |
| `homeboy release` | read-only UI | Release workspace runs `homeboy release <component> --dry-run`. | Dry-run planning is supported. Mutating release execution, recover, deploy-after-release, skip checks/publish, and GitHub release controls remain CLI-only. | Matrix only |
| `homeboy review` | supported first-pass UI | Quality workspace `Run Review` action calls `homeboy review --summary`. | Read-only summary is supported with scope controls. PR-comment output, baselines, and ratchets remain CLI-only. | Matrix only |
| `homeboy audit` | supported first-pass UI | Quality stage button plus audit slice helpers in `Homeboy/Core/CLI/HomeboyCLI+QualityCommands.swift`. | Current `homeboy audit <component>` stage is supported. Baseline/ratchet writes stay CLI-only. | Matrix only |
| `homeboy refactor` | partial/stale | Refactor wrappers still exist in `Homeboy/Core/CLI/HomeboyCLI+QualityCommands.swift`, but no visible workflow. | Keep mutating refactors out of the UI until proposal/confirmation UX exists. Remove or modernize unused wrappers only when a concrete desktop workflow needs them. | Matrix only |
| `homeboy rig` | supported first-pass UI | Rigs workspace in `Homeboy/Views/Rigs/`; wrappers in `Homeboy/Core/CLI/HomeboyCLI+RigBenchReleaseCommands.swift`. | Supports list/show/status/check plus confirmed `up`/`down`. `install`, `update`, `sources`, `sync`, and `app` lifecycle commands remain CLI-only. | Matrix only |
| `homeboy stack` | read-only UI | Stack manager in `Homeboy/App/ContentView.swift`; wrappers in `Homeboy/Core/CLI/HomeboyCLI+StackCommands.swift`. | Supports list/show/status/inspect. `create`, `add-pr`, `remove-pr`, `apply`, `rebase`, `sync`, `push`, and `diff` remain CLI-only until guarded write UX exists. | Matrix only |
| `homeboy undo` | partial/stale | Undo wrappers exist in `Homeboy/Core/CLI/HomeboyCLI+RigBenchReleaseCommands.swift`, but no visible undo surface. | Surface undo from workflows that perform Homeboy writes rather than as a standalone command center. | Matrix only |
| `homeboy auth` | supported first-pass UI | API/Auth workspace in `Homeboy/Views/Components/APIAuthWorkspaceView.swift`; wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Supports project auth status/login/logout. This is separate from the desktop app login session. | Matrix only |
| `homeboy api` | read-only UI | API/Auth safe GET explorer. | `api get` is supported. `post`, `put`, `patch`, and `delete` remain CLI-only until preview/confirmation UX exists. | Matrix only |
| `homeboy upgrade` | supported first-pass UI | CLI update UI in `Homeboy/Views/Settings/GeneralSettingsTab.swift` and launch-time update sheet now use `homeboy upgrade --check` / `homeboy upgrade`. | Install help still shows the Homebrew path, but update execution is no longer Homebrew-specific. | Matrix only |
| `homeboy list` | intentionally CLI-only | Alias for `--help`; no desktop wrapper. | No UI needed. | Matrix only |
| `homeboy cargo` | intentionally CLI-only | No Cargo passthrough UI. | Keep as CLI-only extension convenience. Desktop should expose component-level build/test/validate instead. | Matrix only |
| `homeboy wp` | intentionally CLI-only | WordPress settings/auth surfaces exist, but no WP-CLI passthrough. | Keep as CLI-only unless a WordPress operations console is explicitly designed. | Matrix only |

## Workspace Additions Now Shipped

The workspace PR wave closed the old missing-workflow trackers for Bench, Rigs, Stacks, Git, API/Auth, Quality, and Release/build planning. Those closed issues are intentionally not listed as active follow-up work above.

| Area | Shipped desktop surface | Intentional boundary |
|---|---|---|
| Bench | `BenchView` lists scenarios and runs benchmarks. | Baseline/ratchet, rig comparison, multi-run/concurrency, and settings overrides remain CLI-only. |
| Rigs | `RigsView` lists specs, shows status/check output, and confirms `up`/`down`. | Rig package install/update, sources management, stack sync, and app launcher lifecycle remain CLI-only. |
| Stacks | `StackManagerView` lists specs, shows status, and runs read-only inspection. | Stack mutation/materialization commands remain CLI-only. |
| Git | `GitOperationsView` shows component status and GitHub navigation. | Git writes and issue/PR mutations remain CLI-only. |
| API/Auth | `APIAuthWorkspaceView` supports auth status/login/logout and read-only GET requests. | Mutating API verbs remain CLI-only. |
| Quality | `QualityView` wraps review, triage, audit, lint, test, and validate as non-mutating actions. | Fix/write/baseline/ratchet workflows remain CLI-only. |
| Release/build planning | `ReleaseWorkflowView` shows changes, version, release dry-run, and build. | Actual release execution remains CLI-only. |

## Still-Missing Desktop Workflows

- `homeboy transfer` has no desktop workflow.
- `homeboy issues` has no desktop workflow.
- `homeboy undo` has wrappers but no visible workflow.
- Advanced stack/rig/bench/release/git/API mutation flows are intentionally CLI-only until the app has specific confirmation UX for each risk profile.

## Upstream Audit Detector False-Positive History

Some audit findings previously seen against Homeboy Desktop were detector-shape issues in the upstream Homeboy auditor, not desktop product gaps. The linked upstream issues are closed as completed; if a matching finding reappears, treat it as an upstream-detector regression unless the specific finding reproduces as a real app bug.

- [`unreferenced_export`](https://github.com/Extra-Chill/homeboy/issues/1544): can miss exported entry points referenced by framework registration patterns.
- [`missing_interface`](https://github.com/Extra-Chill/homeboy/issues/1545): can conflate traits, helper types, and interface-like conventions.
- [`missing_registration`](https://github.com/Extra-Chill/homeboy/issues/1546) and [`missing_method`](https://github.com/Extra-Chill/homeboy/issues/1547): can over-apply endpoint-style conventions to supporting types.
- [`naming_mismatch`](https://github.com/Extra-Chill/homeboy/issues/1548): can flag helper/utility files that intentionally do not match a directory's primary suffix.
- [`repeated_field_pattern`](https://github.com/Extra-Chill/homeboy/issues/1549): can misread registration arrays and repeated language idioms.
- [`upstream_workaround`](https://github.com/Extra-Chill/homeboy/issues/1550): can mistake historical design references for active workaround markers.
- [`dead_guard`](https://github.com/Extra-Chill/homeboy/issues/1552): can miss runtime-context guards used by tests, activation paths, or platform bootstraps.

## Active Desktop Follow-Up Issues

No active follow-up issues remain from this parity pass. New desktop work should be filed from a concrete workflow need rather than from the historical parity backlog.
