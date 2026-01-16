# Documentation Management

Homeboy provides AI agents with consistent instructions for documentation generation and maintenance. These docs serve as the single source of truth for documentation standards across all Homeboy-managed projects.

## Core Philosophy

### Single Source of Truth
Code is authoritative. Documentation derives from and adapts to code, never the reverse. Uncommitted changes represent active development that documentation must respect.

### Minimal Content, Maximal Coverage
Document every code component with only essential information. Comprehensive coverage means each discrete component gets documentation. Minimal content means concise explanations, not shallow coverage.

### Present-Tense Only
Document current implementation state. No historical language, no future plans, no version history.

### Code-First Verification
Every documented feature must be verified to exist in code before writing about it. Never document aspirational or planned functionality.

## Documentation Types

### CLAUDE.md / AGENTS.md
AI agent context files. Provide architectural principles, coding standards, and operational instructions for AI agents working in the codebase.

### README.md
Project overview for repository viewers. Standard GitHub format with setup instructions, basic usage, and contributor information.

### /docs Directory
User-facing documentation. Explains how to use the system from an end-user perspective, not how it's built.

## Using Homeboy for Documentation

### Alignment (Existing Docs)
When maintaining existing documentation:
```
homeboy docs documentation/alignment
```

### Generation (New Docs)
When creating documentation from scratch:
```
homeboy docs documentation/generation
```

### Structure Standards
For file organization and naming conventions:
```
homeboy docs documentation/structure
```

### Change Detection
Use `homeboy changes` to identify what code has changed, informing which documentation may be stale.

## Integration with Agent Context Files

Projects using Homeboy can add documentation guidance to their agent context file (CLAUDE.md, AGENTS.md, COPILOT.md, or similar):

```markdown
## Documentation Standards
This project uses Homeboy for documentation management.
Run `homeboy docs documentation/index` for documentation philosophy.
When maintaining docs, follow `homeboy docs documentation/alignment`.
When generating docs, follow `homeboy docs documentation/generation`.
```

This ensures AI agents discover documentation standards through their standard context loading.
