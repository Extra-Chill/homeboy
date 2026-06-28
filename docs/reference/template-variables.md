# Template Variables

Homeboy supports template variables in several contexts. Both single-brace (`{var}`) and double-brace (`{{var}}`) syntaxes are supported everywhere for backward compatibility.

## Contexts

### Extract Commands (`extractCommand`)

Used in component configuration for archive extraction after upload.

**Variables:**
| Variable | Description |
|----------|-------------|
| `{artifact}` / `{{artifact}}` | Uploaded artifact filename (not a path) |
| `{targetDir}` / `{{targetDir}}` | Full target directory path |

**Example:**
```json
{
  "extractCommand": "unzip -o {{artifact}} && rm {{artifact}}"
}
```

### Extension Runtime Commands

Used in extension definitions for runtime execution.

**Variables:**
| Variable | Description |
|----------|-------------|
| `{{entrypoint}}` | Extension entrypoint script |
| `{{args}}` | Runtime arguments |
| `{{extensionPath}}` | Extension installation path |

**Example:**
```json
{
  "runtime": {
    "run_command": "./venv/bin/python3 {{entrypoint}} {{args}}"
  }
}
```

### CLI Tool Templates

Used in extension CLI configurations for wrapping commands.

**Variables:**
| Variable | Description |
|----------|-------------|
| `{{cliPath}}` | Path to CLI executable |
| `{{domain}}` | Target domain |
| `{{sitePath}}` | Site root path |

### Environment Variables

Used in extension runtime environment configuration.

**Variables:**
| Variable | Description |
|----------|-------------|
| `{{extensionPath}}` | Extension installation path |
| `{{projectId}}` | Project identifier |

### Special Extension Variables

Available in extension install/cleanup commands:

| Variable | Description |
|----------|-------------|
| `{{selected}}` | Array of selected rows from `--data` |
| `{{settings.<key>}}` | Extension settings value |
| `{{payload.<key>}}` | Action payload data |
| `{{release.<key>}}` | Release configuration data |

### Deploy Override Commands

Used in extension deploy override configurations.

**Variables:**
| Variable | Description |
|----------|-------------|
| `{{artifact}}` | Artifact filename |
| `{{stagingArtifact}}` | Staging path artifact |
| `{{targetDir}}` | Target directory |
| `{{siteRoot}}` | Site root path |
| `{{cliPath}}` | CLI executable path |
| `{{domain}}` | Target domain |
| `{{allowRootFlag}}` | `--allow-root` when SSH user is root |

## Standard Variables

Available in most template contexts:

| Variable | Description |
|----------|-------------|
| `{{projectId}}` | Project identifier |
| `{{args}}` | Command arguments |
| `{{domain}}` | Target domain |
| `{{sitePath}}` | Site root path |
| `{{cliPath}}` | CLI executable path |
| `{{table}}` | Database table name |
| `{{query}}` | SQL query |
| `{{format}}` | Output format |
| `{{targetDir}}` | Target directory |
| `{{db_host}}` | Database host |
| `{{db_port}}` | Database port |
| `{{db_name}}` | Database name |
| `{{db_user}}` | Database user |
| `{{db_password}}` | Database password |

## Syntax Notes

- Both `{var}` and `{{var}}` are supported in all contexts
- Double-brace syntax (`{{var}}`) is preferred for new configurations
- Single-brace syntax (`{var}`) is maintained for backward compatibility
- Variable names are case-sensitive
