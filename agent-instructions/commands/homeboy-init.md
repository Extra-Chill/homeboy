# `homeboy init`

Initialize a repo for use with Homeboy.

## Rules

1. **Gather ALL context first** - run every context command before making decisions
2. **Read workspace documentation** - CLAUDE.md, README.md, etc. explain the project
3. **Use existing configurations as templates** - derive from what already exists
4. **Only ask when derivation fails** - and when asking, show what was found
5. **Never assume stack/language** - all understanding comes from docs and configs

## Phase 1 — Gather Context (MANDATORY)

Run ALL of these commands before proceeding:

```bash
homeboy context           # Is this managed? What components match?
homeboy server list       # What servers exist?
homeboy project list      # What projects exist?
homeboy component list    # What components exist?
homeboy module list       # What modules are available?
```

Then read workspace documentation:
- Read `CLAUDE.md` if it exists (project instructions, build info, deployment info)
- Read `README.md` if it exists (project overview, setup instructions)
- Look for build scripts (`build.sh`, `Makefile`, etc.) and note what they produce

## Phase 2 — Analyze and Derive

### If `managed: true`
This repo is already configured. The `context` output includes component details and detected gaps.

1. **Review context data** - `matchedComponents` and `components` arrays contain configuration details
2. **Check for gaps** - `components[].gaps` shows missing optional fields with remediation commands
3. **Run gap commands** - Execute suggested commands to complete configuration
4. **Report status** and suggest next steps

### If monorepo root (`managed: false` but `containedComponents` exists)
Components are already registered but the root itself isn't managed.

1. **Review context output** - `components[].gaps` shows detected configuration gaps
2. **Run suggested commands** - Each gap includes the exact remediation command
3. **Skip creation** - Focus on completing existing component configuration

### Component Gaps (Automated Detection)

The `homeboy context` command automatically detects missing optional fields:

| Detected File | Missing Field | Why It Matters |
|--------------|---------------|----------------|
| `build.sh` | `buildCommand` | Required for `homeboy build` |
| `CHANGELOG.md` | `changelogTargets` | Required for `homeboy version-bump` |

Each gap includes the exact command to resolve it.

### If `managed: false` (no containedComponents)
Determine what to create based on gathered context:

**Project indicators** (from workspace docs):
- Documentation describes a deployable environment/site
- Contains server configuration, domain references, database setup

**Component indicators** (from workspace docs):
- Documentation describes a buildable/deployable unit
- Contains version info, changelog, build instructions
- Is a subdirectory of a larger project

If unclear from documentation, ask the user:
> Based on the workspace documentation, I found [summary].
> Is this a **Project** (deployable environment) or a **Component** (build/deploy unit)?

## Phase 3 — Create with Intelligent Defaults

### Creating a Project

1. **name**: Derive from directory name or workspace docs
2. **domain**: ASK (cannot derive locally)
3. **modules**: Show `module list` output, ask which apply based on workspace docs context
4. **serverId**: Auto-select if only one server exists, else show list and ask

```bash
homeboy project create "<name>" <domain> --module <moduleId>
homeboy project show <projectId>
```

### Creating a Component

1. **name**: Derive from directory name or workspace docs
2. **localPath**: Current directory
3. **remotePath**: Derive from:
   - Workspace docs (if deployment path specified)
   - Existing components in target project (show patterns)
   - Ask with examples if cannot derive
4. **buildArtifact**: Derive from:
   - Workspace docs (if build output specified)
   - Existing build scripts (what they produce)
   - Ask if cannot derive
5. **buildCommand**: Derive from existing scripts or workspace docs
6. **projectId**: Show `project list`, ask which project this belongs to

```bash
homeboy component create "<name>" --local-path "." --remote-path "<remotePath>" --build-artifact "<buildArtifact>"
homeboy component show <componentId>
```

If versioning/build are relevant (from workspace docs):
```bash
homeboy component set <componentId> --version-target "<file>" --build-command "<command>"
```

## Phase 4 — Verify and Report

1. Run `homeboy context` - confirm `managed: true`
2. Report what was created with all derived values
3. Suggest next steps:
   - Project: `homeboy deploy <projectId> --all`
   - Component: `homeboy build <componentId>` or `homeboy version show <componentId>`

## Example: Smart Questioning

When you must ask, provide context:

**Bad:**
> What is the remotePath for this component?

**Good:**
> I examined 3 existing components in the "myproject" project:
> - component-a: path/to/component-a
> - component-b: path/to/component-b
> - component-c: path/to/component-c
>
> Based on this pattern, where should this component deploy?
> Suggested: path/to/<this-component-name>
