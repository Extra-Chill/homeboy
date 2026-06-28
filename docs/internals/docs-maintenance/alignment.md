# Documentation Alignment

Instructions for keeping existing `.md` documentation synchronized with current codebase implementation.

## Scope

Alignment covers documentation that describes current implementation only. Planning documents (describing future work, architectural plans, etc.) are excluded.

This includes:
- CLAUDE.md / AGENTS.md files
- README.md files
- `/docs` directory contents
- API documentation in `.md` format
- Any other `.md` files in the codebase

## Core Rules

### Never Create New Directory Structures
Work within existing documentation structures only. You may create missing `.md` files within existing directories to fill gaps, but never create new top-level documentation directories.

### Never Modify Code
Documentation alignment is read-only with respect to code files. If you detect code issues, note them but do not modify code.

### Minimal Intervention
Only update what needs correction. Preserve accurate existing content. Do not rewrite documentation that is already correct.

## Workflow

### 1. Build Source Context
```sh
homeboy docs map <component>
```

Use the map output plus targeted source reads to understand the current code shape before editing documentation.

### 2. Find Current-Workflow References
Search the relevant documentation for command names, file paths, configuration keys, and workflow steps that may have drifted from the current implementation.

### 3. Fix Broken References
For each stale reference:
- Read the current source or CLI help that owns the behavior
- Update or remove the reference without inventing replacement workflows
- Preserve historical changelog entries unless they are presented as current workflow

### 4. Verify Code Examples

### 5. Verify Changes
```sh
homeboy audit <component>
```
Run focused grep/source checks and repository quality gates that cover the changed docs. If `homeboy audit` reports documentation-reference findings, fix them before completion.

## Forbidden Content

Never generate these during alignment:
- Installation guides or setup instructions
- Getting started tutorials
- Troubleshooting sections
- Configuration walkthroughs
- Generic workflow examples
- Version history or changelog content
- Marketing copy

## Forbidden Actions

- Never use `git checkout`, `git reset`, or any command that reverts code
- Never modify non-`.md` files
- Never revert or undo code changes
- Ignore uncommitted code changes - they represent active development

## Gap-Filling Within Existing Structures

When existing directories have coverage gaps:

1. **Analyze Structure**: Examine existing directory organization and naming patterns
2. **Identify Gaps**: Find codebase features without corresponding documentation
3. **Create Files**: Add missing `.md` files within existing directory structure
4. **Follow Patterns**: Match existing naming conventions and organizational hierarchy

**Allowed**: Creating `.md` files within existing `/docs` directory
**Forbidden**: Creating new top-level documentation directories

## Quality Gates

Before completion, verify:
- All documented features exist in current codebase
- All outdated information is corrected or removed
- Present-tense language throughout
- Cross-references are accurate and functional
- No new directory structures were created
