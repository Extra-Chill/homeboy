# Documentation Alignment

Instructions for keeping existing `.md` documentation synchronized with current codebase implementation.

## Scope

Alignment covers ALL existing `.md` files:
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

### 1. Detect Changes
Run `homeboy changes` to identify what code has changed. This informs which documentation may be stale.

### 2. Discover Documentation
Find all `.md` files in the codebase:
- Respect `.gitignore` and `.buildignore` exclusions
- Exception: Always include `/docs` directory regardless of ignore patterns

### 3. Verify Accuracy
For each documented feature:
- Verify the feature exists in current code
- Check that documented behavior matches implementation
- Confirm code paths and file references are accurate

### 4. Update Stale Content
When documentation conflicts with code:
- Update documentation to match code (code is authoritative)
- Use present-tense language
- Remove references to deleted functionality
- Add documentation for new functionality within existing structure

### 5. Cross-Reference Validation
Ensure consistency across all `.md` files:
- File paths referenced should exist
- Function/class names should be accurate
- Architectural descriptions should match implementation

## Link Policy

WordPress handles documentation navigation, not inline markdown links.

**Remove**:
- Internal `.md` links between documentation files
- Example: `[Settings](admin/settings.md)` → `Settings`

**Preserve**:
- Anchor links within documents: `[Jump to section](#section)`
- External URLs: `[WordPress Codex](https://codex.wordpress.org)`

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
