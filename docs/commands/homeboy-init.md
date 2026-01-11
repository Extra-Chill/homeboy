# `homeboy init` / `/homeboy-init`

Initialize a repo for use with Homeboy.

This document is a single source of truth:

- `homeboy init` prints this page.
- `/homeboy-init` is a symlink to this page.

## Rules

- Only run `homeboy *` commands.
- Do not invent IDs or flags.
- If an identifier or value is required and cannot be derived from Homeboy output, ask the user for it explicitly.

## Step 0 — Inspect current Homeboy state (no writes yet)

Run:

1. `homeboy doctor scan --scope all --fail-on error`
2. `homeboy project list`
3. `homeboy project list --current`
4. `homeboy component list`
5. `homeboy module list`

## Choose a scope

Prefer component-scoped initialization unless the user explicitly intends project-level remote operations.

Choose:

- **Project**: repo represents a deployable environment and user intends remote ops (`ssh`, `wp`, `db`, `deploy`).
- **Component**: repo (or subdirectory) is a build/version/deploy unit.
- **Module**: repo is meant to be installed and run as a Homeboy module.

If multiple scopes could apply, ask the user which scope they intend.

## Project initialization

Goal: ensure the project exists, is active, and has valid config.

If the project already exists:

1. `homeboy project show <projectId>`
2. `homeboy project switch <projectId>`
3. `homeboy project repair <projectId>`

If the project does not exist:

1. Ask for: project `name`, `domain`, `project_type` (e.g. `wordpress`), optional `serverId`, optional `basePath`, optional `tablePrefix`.
2. Create (activate if desired):
   - `homeboy project create "<name>" <domain> <project_type> --activate`
3. Apply any missing config:
   - `homeboy project set <projectId> --domain <domain> --server-id <serverId>`
4. Repair:
   - `homeboy project repair <projectId>`

Verify:

- `homeboy project list --current`
- `homeboy project show <projectId>`

## Component initialization

Goal: ensure a correct component configuration exists for the repo (or selected subdirectory).

1. Decide which component should represent this repo.
   - If you don’t know the intended `componentId`, ask the user.
2. If the component exists:
   - `homeboy component show <componentId>`
   - Fix missing/incorrect values with:
     - `homeboy component set <componentId> ...`
3. If the component does not exist:
   - Ask for required values: `name`, `localPath`, `remotePath`, `buildArtifact`.
   - Create:
     - `homeboy component create "<name>" --local-path "<localPath>" --remote-path "<remotePath>" --build-artifact "<buildArtifact>"`
4. If versioning/build are relevant, ensure the component is configured with appropriate `--version-target ...` and `--build-command ...`.

Verify readiness (only when configured):

- `homeboy version show <componentId>`
- `homeboy build <componentId>`

## Module initialization

Goal: ensure the module is installed and runnable.

1. Check what’s installed:
   - `homeboy module list`
2. If the module is missing, ask for:
   - module git URL
   - desired module id (optional; Homeboy can derive but explicit is better)
3. Install/update:
   - `homeboy module install <git_url> --id <moduleId>`
   - `homeboy module update <moduleId>`
4. Verify:
   - `homeboy module list`
   - `homeboy module run <moduleId> --help`

## Success checklist

Report what was initialized and what to run next:

- Project: active `projectId` is set, and `doctor scan` has no errors.
- Component: `componentId` exists and `version`/`build` commands succeed (when applicable).
- Module: module installs/updates, and `module run` works.

Suggested next steps:

- Project: `homeboy deploy <projectId> --dry-run --all`
- Component: `homeboy version bump <componentId> patch --changelog-add "..."`
- Module: `homeboy module setup <moduleId>`
