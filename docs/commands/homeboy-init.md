# `homeboy init`

Initialize a repo for use with Homeboy.

## Rules

- Only run `homeboy *` commands.
- Do not invent IDs or flags.
- If a value is required and cannot be derived, ask the user.

## Step 1 — Check if this directory is managed

Run:

1. `homeboy context`

### If `managed: false` (UNMANAGED REPO)

This is a NEW repo. Skip to "New Project/Component Initialization" below.
Do NOT run project list or component list - they are irrelevant for unmanaged repos.

### If `managed: true` (EXISTING REPO)

This repo is already configured. The `matchedComponents` array tells you which component(s) are associated.
Proceed to "Existing Configuration Verification" below.

---

## New Project/Component Initialization

For unmanaged repos, determine what to create:

### Choose a scope

Based on the current repo structure:

- **Project**: repo is a deployable environment (for example: a web app) with associated components.
- **Component**: repo (or subdirectory) is a build/version/deploy unit within a project.

If unclear which scope applies, ask the user.

### Creating a new project

1. Ask for: `name`, `domain`, module IDs to enable (e.g. `wordpress`), optional `serverId`
2. Create:
   - `homeboy project create "<name>" <domain> --module <moduleId>`
3. Verify:
   - `homeboy project show <projectId>`

### Creating a new component

1. Ask for: `name`, `remotePath`, `buildArtifact`
2. Create:
   - `homeboy component create "<name>" --local-path "." --remote-path "<remotePath>" --build-artifact "<buildArtifact>"`
3. Verify:
   - `homeboy component show <componentId>`
4. If versioning/build are relevant, configure:
   - `homeboy component set <componentId> --version-target "<file>" --build-command "<command>"`

---

## Existing Configuration Verification

For managed repos (`managed: true`), verify and repair existing configuration:

1. `homeboy component show <matchedComponentId>`
2. If issues found:
   - `homeboy component set <componentId> ...` to fix missing/incorrect values
3. Verify build (if configured):
   - `homeboy build <componentId>`

---

## Success Checklist

Report what was initialized and suggest next steps:

- **Unmanaged → Created project/component**: `homeboy context` now shows `managed: true`
- **Managed → Verified/repaired**: component commands succeed

### Suggested next steps

- Project: `homeboy deploy <projectId> --dry-run --all`
- Component: `homeboy changelog add <componentId> "..."`, then `homeboy version bump <componentId> patch`
