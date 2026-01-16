# Documentation Generation

Instructions for creating comprehensive user-facing documentation from scratch by analyzing the codebase.

## Purpose

Generate structured documentation in the `/docs` directory that helps end users understand and use the system. This is technical user documentation that is designed to be human-readable.

## Core Principles

### Code-First Verification
Every documented feature must be verified to exist in code. Never document aspirational functionality.

### Minimal Content, Maximal Coverage
Document every code component (function, class, API, hook, configuration) with only essential information. Comprehensive coverage means each discrete component gets dedicated documentation. Minimal content means concise explanations.

### Modular Component Documentation
Each discrete component gets its own dedicated file with complete coverage of all its methods, properties, and functionality.

### Code Structure Mirroring
Documentation structure must directly mirror actual code organization. Each subdirectory corresponds to actual code modules.

### End User Focus
Write for people who need to USE the system, not build it. Make technical concepts accessible.

## Workflow

### 1. Code Structure Analysis
Map the actual file, class, and function organization. Use comprehensive scanning to understand the codebase architecture.

### 2. Component Discovery
Continue systematic search until ALL handlers, tools, APIs, filters, actions, and components are cataloged. Do not proceed until complete inventory is achieved.

### 3. Entry Point Identification
Distinguish main user-facing APIs from internal helper functions. Prioritize documentation of user-facing functionality.

### 4. Usage Pattern Extraction
Find actual usage workflows in existing code, tests, and examples. Document real patterns, not theoretical usage.

### 5. Create Directory Structure
Create subdirectories in `/docs` that directly correspond to actual code modules and components.

### 6. Create Documentation Files
Use `homeboy docs scaffold` to create the file structure with H1 titles, or create files manually following the structure standards in `homeboy docs documentation/structure`.

### 7. Write Content
For each file:
- Document all methods, properties, configuration options within that component's scope
- Include only complete code examples from actual implementation
- Show actual usage patterns found in existing code

### 8. Coverage Validation
Verify ALL discovered components have been documented before completion.

## Forbidden Content

Never generate:
- Installation guides or setup instructions
- Getting started tutorials or step-by-step guides
- Troubleshooting sections or common issues
- Configuration walkthroughs or setup wizards
- Generic workflow examples or use case scenarios
- Version history or changelog content
- Marketing copy or feature promotion
- Comprehensive explanations of basic concepts

## File Naming

- Use descriptive filenames that reflect functionality being documented
- No generic names like `readme.md` in subdirectories
- File names describe what the content covers

## Completion Criteria

Do not mark generation complete until:
- Every discoverable handler, tool, API, filter, action, and component has dedicated documentation
- All code modules identified in discovery phase have corresponding documentation files
- All subdirectories in `/docs` correspond to actual code modules
- All documented workflows correspond to actual code patterns

## Quality Gates

Before completion, verify:
- All documented features verified to exist in current codebase
- Present-tense language throughout
- All code modules have corresponding documentation
- User-focused explanations that remain technically accurate
- Complete usage examples from actual implementation
- Hierarchical structure mirrors actual code organization
