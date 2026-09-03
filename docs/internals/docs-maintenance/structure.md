# Documentation Structure

Standard patterns for organizing documentation files and directories. These
rules describe Homeboy's own `docs/` tree; downstream projects managed by
Homeboy may differ.

## Directory Conventions

### /docs Directory
User-facing documentation lives in `/docs` at the project root. This directory is always included in documentation scans regardless of `.gitignore` patterns.

### Subdirectory Organization
Group subdirectories by audience and purpose, and let each one carry an entry
point. Homeboy's own tree:

```
docs/
├── index.md                # Entry point for the whole tree
├── commands/               # Hand-written command narrative
│   ├── index.md
│   └── agent-task.md
├── reference/              # Generated reference material
│   └── index.md
├── concepts/               # Conceptual explanation
│   └── index.md
├── workflows/              # Task-oriented guides
│   └── index.md
├── operations/             # Running Homeboy
│   └── index.md
└── internals/              # For people maintaining Homeboy itself
    ├── index.md
    └── docs-maintenance/index.md
```

`docs/commands/*.md` contains hand-written concepts and recipes. Exact command
syntax comes directly from Clap through `homeboy <command> --help`; Homeboy does
not check in a second generated Markdown projection of that surface.

### Hierarchical Depth
Match the depth of code organization. If code has nested extensions, documentation can have nested subdirectories. Avoid unnecessary nesting.

## File Naming

### Descriptive Names
File names describe the functionality being documented:
- `authentication.md` not `auth.md`
- `user-management.md` not `users.md`
- `form-validation.md` not `forms.md`

### Descriptive Names For Content Files
Every file that documents something specific gets a specific name. Do not use
`readme.md` or `overview.md` inside `docs/`.

### Directory Entry Points
When a directory needs an introductory file (what lives here, how the pieces
connect, where to go next), name it `index.md`:

```
internals/
├── index.md               # Entry point: "What is in internals?"
├── docs-maintenance/
│   └── index.md           # Entry point for docs-maintenance
└── developer-guide/
    └── architecture-cleanup-map.md
```

`{directory}/index.md` is the convention throughout this tree, and it is what
`docs/index.md` and `README.md` link to. An earlier revision of this file banned
`index.md` in subdirectories; that rule never matched the repository and is
withdrawn.

### Kebab-Case
Use kebab-case for all file names: `user-authentication.md`, `api-reference.md`

## File Structure

### H1 Title
Every documentation file starts with a single H1 title describing what the file covers:

```markdown
# Configuration Precedence Map

Content about how overlapping config schemas resolve...
```

### Section Headers
Use H2 for major sections, H3 for subsections:

```markdown
# Runner Contract

## Step Filtering

### Include Semantics

### Skip Semantics

## Environment Mapping
```

### Code Examples
Include code examples from actual implementation. Use appropriate language hints — in this repository that is usually `bash` for command usage and `rust` or `json` for contracts:

````markdown
```bash
homeboy agent-task status <run-id>
```
````

## Content Organization

### Component Files
For component documentation, organize by:
1. Overview (what the component does)
2. Properties/Methods (complete listing)
3. Usage (code examples from actual implementation)

### API Documentation
For API endpoint documentation, organize by:
1. Endpoint (method and path)
2. Authentication requirements
3. Parameters
4. Response format
5. Example request/response

### Configuration Documentation
For configuration documentation, organize by:
1. Option name
2. Type and default value
3. Description
4. Valid values

## Exclusions from /docs

These belong elsewhere, not in `/docs`:
- CLAUDE.md / AGENTS.md (project root)
- README.md (project root or component roots)
- Build documentation (in code comments or separate dev docs)

`docs/changelog.md` is the exception: it lives in `/docs` because `homeboy release` generates it. Never hand-edit it.

## Documentation Commands

Use `homeboy self docs <topic>` to read embedded guidance and `homeboy self docs map <component>` to generate a machine-optimized codebase map. Create or edit documentation manually against the current source, then verify with focused source checks plus `homeboy review audit` or `homeboy review lint` where those commands apply.
