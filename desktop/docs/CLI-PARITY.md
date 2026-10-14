# Homeboy CLI Parity Matrix

This matrix maps the current `homeboy` CLI command surface to Homeboy Desktop support.
Desktop now has two layers of CLI parity:

- **Guided workflow UI** for common command families with safer forms, previews, and focused output.
- **Command Browser** for full command-surface discoverability, help output, copyable invocations, and explicit raw CLI execution.

Source of truth checked for this pass: `homeboy --help` and `homeboy contract manifest` from installed `homeboy` 0.421.0.

## Status Legend

- **supported first-pass UI**: the desktop app exposes the workflow with current CLI wrappers, but may intentionally omit advanced flags.
- **read-only UI**: the desktop app exposes inspection/status output only; mutating commands stay in the CLI.
- **partial/stale**: a desktop surface exists, but some wrappers or models still need cleanup.
- **intentionally CLI-only**: the command is terminal/operator-shaped and should not get desktop UI without a stronger product need.
- **missing workflow**: no guided workflow exists yet; use the Command Browser or CLI for the raw command.

## Matrix

| CLI command | Desktop support status | Current app surface / file references | Desktop boundary / remaining work | Tracking |
|---|---|---|---|---|
| `homeboy project` | supported first-pass UI | Project switcher and settings-backed project CRUD; wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Usable as app infrastructure. Raw global config editing stays CLI-only. | Matrix only |
| `homeboy ssh` | partial/stale | Generic SSH support behind file and deploy workflows; server settings in `Homeboy/Views/Settings/ServersSettingsTab.swift`. | Keep as supporting infrastructure. Add an ad-hoc SSH console only if a real desktop workflow needs it. | Matrix only |
| `homeboy server` | supported first-pass UI | Server settings UI plus server wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Usable for normal server setup. Advanced SSH/key diagnostics stay CLI-only until a focused UI needs them. | Matrix only |
| `homeboy bench` | supported first-pass UI | Bench workspace in `Homeboy/Views/BenchView.swift`; wrappers in `Homeboy/Core/CLI/HomeboyCLI+RigBenchReleaseCommands.swift`. | Lists scenarios and runs benchmarks with iteration control. Baseline, ratchet, multi-run, concurrency, settings, and `--rig` comparisons remain CLI-only. | Matrix only |
| `homeboy file` | supported first-pass UI | Remote file editor in `Homeboy/Extensions/RemoteFileEditor/`; file wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Core list/read/write/delete/rename/search workflows exist. Keep destructive operations guarded. | Matrix only |
| `homeboy transfer` | missing workflow | No dedicated transfer workflow. | Keep out of File Editor for now. If the command returns to the installed CLI surface, Command Browser will expose it from `homeboy --help`. | Matrix only |
| `homeboy deploy` | supported first-pass UI | Deployer tool in `Homeboy/Extensions/Deployer/`. | Deploy remains its own guarded write workflow. Release/build planning is now separate from deployment. | Matrix only |
| `homeboy component` | supported first-pass UI | Component settings UI and models; wrappers in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Usable for normal component config. Bulk/JSON-pointer style edits stay CLI-only. | Matrix only |
| `homeboy config` | intentionally CLI-only | App settings expose focused project/server/component fields, not raw global config mutation. | Keep raw global config editing in the CLI. | Matrix only |
| `homeboy daemon` | intentionally CLI-only | No daemon management UI. | Desktop consumes CLI/API behavior; it should not manage the local daemon lifecycle unless a future diagnostic screen needs it. | Matrix only |
| `homeboy extension` | supported first-pass UI | Extension manager and dynamic extension views under `Homeboy/Core/Extensions/` and `Homeboy/Views/Extensions/`. | Extension install/setup/run exists as app infrastructure. Advanced extension authoring stays CLI/docs-driven. | Matrix only |
| `homeboy status` | supported first-pass UI | Workspace discovery now uses `status --full` in `Homeboy/Core/CLI/HomeboyCLI+WorkspaceCommands.swift`. | Primary app bootstrap path is current. A richer top-level status dashboard can wait for product need. | Matrix only |
| `homeboy git` | read-only UI | Git workspace in `Homeboy/Extensions/GitOperations/`; `gitStatus()` wrapper plus GitHub issue/PR navigation. | Read-only `git status` is supported. `commit`, `push`, `pull`, `rebase`, `cherry-pick`, `tag`, `issue`, and `pr` writes stay CLI-only until confirmation UX exists. | Matrix only |
| `homeboy release` | read-only UI | Release workspace runs `homeboy release <component> --dry-run`, `homeboy release changes`, and `homeboy release version show`. | Dry-run planning is supported. Mutating release execution, recover, deploy-after-release, skip checks/publish, and GitHub release controls remain CLI-only. | Matrix only |
| `homeboy review` | supported first-pass UI | Quality workspace runs `homeboy review --summary`, with lint and test deep-dives via `homeboy review lint` and `homeboy review test`. Release planning runs `homeboy review build`. | Read-only summary and stage runs are supported with scope controls. PR-comment output, baselines, and ratchets remain CLI-only. | Matrix only |
| `homeboy refactor rename` | intentionally CLI-only | The supported rename wrapper lives in `Homeboy/Core/CLI/HomeboyCLI+QualityCommands.swift`. | Keep mutating renames in the CLI until confirmation UX exists. | Matrix only |
| `homeboy rig` | supported first-pass UI | Rigs workspace in `Homeboy/Views/Rigs/`; wrappers in `Homeboy/Core/CLI/HomeboyCLI+RigBenchReleaseCommands.swift`. | Supports list/show/status/check plus confirmed `up`/`down`. `install`, `update`, `sources`, `sync`, and `app` lifecycle commands remain CLI-only. | Matrix only |
| `homeboy upgrade` | supported first-pass UI | CLI update UI in `Homeboy/Views/Settings/GeneralSettingsTab.swift` and launch-time update sheet now use `homeboy upgrade --check` / `homeboy upgrade`. | Install help still shows the Homebrew path, but update execution is no longer Homebrew-specific. | Matrix only |
| `homeboy cargo` | intentionally CLI-only | No Cargo passthrough UI. | Keep as CLI-only extension convenience. | Matrix only |
| `homeboy wp` | intentionally CLI-only | WordPress settings surfaces exist, but no WP-CLI passthrough. | Keep as CLI-only unless a WordPress operations console is explicitly designed. | Matrix only |

## Workspace Additions Now Shipped

The workspace PR wave closed the old missing-workflow trackers for Bench, Rigs, Git, Quality, and Release/build planning. Those closed issues are intentionally not listed as active follow-up work above.

| Area | Shipped desktop surface | Intentional boundary |
|---|---|---|
| Commands | `CommandBrowserView` lists the CLI command catalog, shows command help, copies invocations, and runs explicit `homeboy ...` commands through the CLI bridge. | Guided workflow tabs remain the safer path for common operations; Command Browser is the escape hatch for raw CLI parity. |
| Bench | `BenchView` lists scenarios and runs benchmarks. | Baseline/ratchet, rig comparison, multi-run/concurrency, and settings overrides remain CLI-only. |
| Rigs | `RigsView` lists specs, shows status/check output, and confirms `up`/`down`. | Rig package install/update, sources management, and app launcher lifecycle remain CLI-only. |
| Git | `GitOperationsView` shows component status and GitHub navigation. | Git writes and issue/PR mutations remain CLI-only. |
| Quality | `QualityView` runs `review`, plus `review lint` and `review test` deep-dives. | Fix/write/baseline/ratchet workflows remain CLI-only. |
| Release planning | `ReleaseWorkflowView` shows `release changes`, `release version show`, release dry-run, and `review build`. | Actual release execution remains CLI-only. |

## Still-Missing Desktop Workflows

- `homeboy transfer` is not in the current CLI and has no desktop workflow.
- Issue reconciliation is `homeboy git issue`, not a top-level issues command, and stays CLI-only.
- Advanced rig/bench/release/git mutation flows are intentionally CLI-only until the app has specific confirmation UX for each risk profile.

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
