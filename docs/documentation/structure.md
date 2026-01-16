# Documentation Structure

Standard patterns for organizing documentation files and directories.

## Directory Conventions

### /docs Directory
User-facing documentation lives in `/docs` at the project root. This directory is always included in documentation scans regardless of `.gitignore` patterns.

### Subdirectory Organization
Create subdirectories that mirror actual code modules:

```
docs/
├── api/                    # REST API documentation
│   ├── authentication.md   # Auth endpoints
│   └── users.md           # User endpoints
├── components/            # UI component documentation
│   ├── forms.md           # Form components
│   └── navigation.md      # Navigation components
└── configuration/         # Configuration documentation
    └── settings.md        # Application settings
```

### Hierarchical Depth
Match the depth of code organization. If code has nested modules, documentation can have nested subdirectories. Avoid unnecessary nesting.

## File Naming

### Descriptive Names
File names describe the functionality being documented:
- `authentication.md` not `auth.md`
- `user-management.md` not `users.md`
- `form-validation.md` not `forms.md`

### No Generic Names
Never use generic names like `readme.md`, `index.md`, or `overview.md` in subdirectories. Each file should have a specific, descriptive name.

### Kebab-Case
Use kebab-case for all file names: `user-authentication.md`, `api-reference.md`

## File Structure

### H1 Title
Every documentation file starts with a single H1 title describing what the file covers:

```markdown
# User Authentication

Content about user authentication...
```

### Section Headers
Use H2 for major sections, H3 for subsections:

```markdown
# User Authentication

## Login Methods

### Email/Password

### OAuth Providers

## Session Management
```

### Code Examples
Include code examples from actual implementation. Use appropriate language hints:

```markdown
```php
$user = get_user_by('email', $email);
```
```

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
- CHANGELOG.md (project root)
- Build documentation (in code comments or separate dev docs)

## Scaffold Command

Use `homeboy docs scaffold` to analyze the codebase. The command:
1. Scans source directories
2. Lists existing documentation files
3. Identifies potentially undocumented areas
4. Returns instructions for next steps

This is read-only analysis. Use `homeboy docs generate` to create files.
