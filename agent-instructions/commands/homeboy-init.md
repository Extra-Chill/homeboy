---
name: homeboy-init
description: Initialize a repo as a Homeboy project, component, or module (choose intelligently).
version: 0.1.0
allowed-tools: Bash(homeboy *)
---

# Homeboy init

Initialize the current working directory in Homeboy with the minimum required configuration.

Choose the correct initialization scope:
- **Project**: a deployable app/site with server + remote operations
- **Component**: a buildable/deployable unit (standalone or within a project)
- **Module**: a Homeboy module managed via `homeboy module ...`

Do not invent IDs. If ambiguous, ask for the missing `projectId`, `componentId`, or module details.

## Step 0 — Inspect current Homeboy state (no writes yet)

Run:

1. `homeboy project list`
2. `homeboy project list --current`
3. `homeboy doctor`

If relevant:
- `homeboy component list`
- `homeboy module list`

## Decide scope (project vs component vs module)

### Choose **Project** when
- The repo represents a deployable environment (e.g. WordPress site/app) and should support `ssh/wp/db/deploy`.

If you choose Project, also initialize at least one Component (the deployable unit).

### Choose **Component** when
- The repo (or a subdirectory) is a build/version/deploy unit, but not a full project environment.

### Choose **Module** when
- The repo is intended to be installed/run as a Homeboy module.

If multiple scopes could apply, ask which one the user intends.

## Project init

Goal: ensure the project exists, is active, and is repairable.

1. If the project already exists:
   - `homeboy project show <projectId>`
   - `homeboy project switch <projectId>`
   - `homeboy project repair <projectId>`

2. If the project does NOT exist:
   - Ask for: project name, domain, type (e.g. `wordpress`), and server id (or whether to create a server first).
   - Create + activate:
     - `homeboy project create "<name>" <domain> <type> --activate`
   - Configure:
     - `homeboy project set <projectId> --domain <domain> --server-id <serverId>`
   - Repair:
     - `homeboy project repair <projectId>`

3. Verify:
   - `homeboy project list --current`
   - `homeboy project show <projectId>`

## Component init

Goal: ensure a correct component configuration exists for the current repo (or selected subdirectory).

1. Determine which component to initialize.
   - If you don’t know the intended `componentId`, ask.

2. If the component already exists:
   - `homeboy component show <componentId>`
   - If settings are missing or incorrect, fix via `homeboy component set ...` (use `homeboy help component set` to find exact flags).

3. If the component does NOT exist:
   - Create it via `homeboy component create ...` (use `homeboy help component create` for exact args).
   - Immediately verify:
     - `homeboy component show <componentId>`

4. If versioning/build are relevant, ensure the component is configured with:
   - `version_file` (and optional `version_pattern`)
   - `build_command`

5. Verify readiness (only when configured):
   - `homeboy version show <componentId>`
   - `homeboy build <componentId>`

## Module init

Goal: ensure the module is sourced/installed and runnable.

1. Check whether it is already present:
   - `homeboy module list`

2. If module is missing, ask for:
   - module source (git URL or local path)
   - desired module id

3. Configure/install/update using the existing module commands (use `homeboy help module`):
   - `homeboy module source ...`
   - `homeboy module install ...`
   - `homeboy module update ...`

4. Verify:
   - `homeboy module list`
   - `homeboy module run <moduleId> --help` (or the expected entrypoint)

## Success checklist (report back)

Report what was initialized and how to use it next:

- Project: active `projectId` and server linkage verified
- Component: `componentId`, local path, version/build readiness verified
- Module: module id/source and runnable command verified

Suggested next steps:
- Project: `homeboy deploy <projectId> --dry-run --all`
- Component: `homeboy version bump ...`, `homeboy git status/commit/push ...`, `homeboy build ...`
- Module: `homeboy module run ...`
