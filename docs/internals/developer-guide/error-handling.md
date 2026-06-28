# Error Handling Patterns

Homeboy uses a centralized error system for consistent error reporting across all commands.

## Error Categories

Errors are categorized by type to provide context about what went wrong:

### Validation Errors

Errors related to input validation and schema compliance:

- **Invalid argument**: Argument value is not allowed
- **Missing required field**: Required configuration field is missing
- **Schema violation**: JSON does not match expected schema
- **Version format invalid**: Version string does not follow semantic versioning

**Example:**
```bash
homeboy component create --local-path /path/to/component --remote-path /path
# Error: build_artifact is required
```

### I/O Errors

Errors related to file and network operations:

- **File not found**: Configuration file or path does not exist
- **Permission denied**: Insufficient permissions for file operation
- **Network connection failed**: Unable to connect to server or API
- **SSH connection failed**: Unable to establish SSH connection

**Example:**
```bash
homeboy component show nonexistent
# Error: Component not found: nonexistent
```

### Extension Errors

Errors related to extension execution:

- **Extension not found**: Extension ID not in extensions directory
- **Extension execution failed**: Extension returned non-zero exit code
- **Extension not ready**: Extension ready_check failed
- **Extension manifest invalid**: Extension manifest has errors

**Example:**
```bash
homeboy extension run nonexistent --project mysite
# Error: Extension not found: nonexistent
```

### Configuration Errors

Errors related to configuration management:

- **Configuration not found**: Configuration file does not exist
- **Configuration parse failed**: Invalid JSON syntax
- **Configuration merge failed**: Conflicting or incompatible configuration

**Example:**
```bash
homeboy component set mycomponent --json 'invalid json'
# Error: Failed to parse JSON: unexpected character
```

### Internal Errors

Unexpected errors in Homeboy implementation:

- **Other**: Uncategorized internal errors
- **Panic**: Rust panic (should never occur)

## Error Context

Errors include contextual information to aid debugging:

- **Field name**: Which field caused validation error
- **Field value**: The problematic value
- **File path**: Which file caused I/O error
- **Component ID**: Which component is being processed
- **Project ID**: Which project is being processed
- **Extension ID**: Which extension is being executed
- **Command**: Which command failed
- **Underlying error**: Original error message

## Error Output

### Human-Readable Output

Errors are displayed with clear messages:

```
Error: Component not found: mycomponent
```

Contextual information follows:

```
  • Component ID: mycomponent
  • Searched in: /home/user/.config/homeboy/components/
```

### JSON Output

Errors are wrapped in JSON envelope:

```json
{
  "success": false,
  "error": {
    "category": "validation",
    "type": "not_found",
    "message": "Component not found: mycomponent",
    "context": {
      "component_id": "mycomponent",
      "search_path": "/home/user/.config/homeboy/components/"
    }
  }
}
```

## Error Recovery Strategies

### Validation Errors

**Cause:** Invalid input or configuration

**Recovery:**
1. Review error message for specific issue
2. Check field values and format
3. Reference documentation for correct schema
4. Retry command with corrected input

**Example:**
```bash
homeboy component create --local-path /path/to/component --remote-path /path
# Error: build_artifact is required

# Recovery:
homeboy component create \
  --local-path /path/to/component \
  --remote-path /path \
  --build-artifact build/component.zip
```

### I/O Errors

**Cause:** File system or network issues

**Recovery:**
1. Verify file paths exist and are accessible
2. Check file permissions
3. Verify network connectivity
4. Check server availability

**Example:**
```bash
homeboy deploy mysite
# Error: SSH connection failed: Connection refused

# Recovery:
1. Check server is running: ping myserver.com
2. Verify SSH port (default 22) is open
3. Check server configuration
```

### Extension Errors

**Cause:** Extension execution issues

**Recovery:**
1. Verify extension is installed: `homeboy extension list`
2. Check extension manifest for errors
3. Run extension setup: `homeboy extension setup <extension_id>`
4. Review extension output for specific error
5. Check extension dependencies

**Example:**
```bash
homeboy extension run python-script --project mysite
# Error: Extension execution failed: exit code 1

# Recovery:
1. Check extension logs for error details
2. Verify dependencies: python3 -m pip install -r requirements.txt
3. Test extension locally
```

### Configuration Errors

**Cause:** JSON parsing or merge issues

**Recovery:**
1. Validate JSON syntax
2. Check for typos in field names
3. Use JSON linter/formatter
4. Restore from backup if available
5. Re-create configuration via CLI

**Example:**
```bash
homeboy component set mycomponent --json '{ "version": 1.2.3 }'
# Error: Failed to parse JSON: trailing comma

# Recovery:
homeboy component set mycomponent --json '{ "version": "1.2.3" }'
```

## Error Prevention

### Input Validation

Before executing commands, Homeboy validates:
- Required fields are present
- Values match expected types
- Paths exist and are accessible
- IDs reference existing configurations

### Schema Validation

Configuration files are validated against schemas on load:
- Component schema
- Project schema
- Server schema
- Extension manifest schema

### Pre-flight Checks

Commands that perform destructive operations run checks:
- SSH connectivity before deploy
- Version detection before version bump
- Component existence before delete

### Safe Defaults

Homeboy uses safe defaults:
- No implicit overwrites (requires explicit flags)
- Dry-run mode for preview
- Backups before destructive operations

## Common Error Scenarios

### SSH Connection Failures

**Error:** `SSH connection failed`

**Common causes:**
- Server is down or unreachable
- Wrong SSH port specified
- Network firewall blocking connection
- SSH service not running on server

**Resolution:**
1. Verify server connectivity: `ping server.com`
2. Check SSH port: `telnet server.com 22`
3. Verify server configuration
4. Check firewall rules

### Extension Not Found

**Error:** `Extension not found: <extension_id>`

**Common causes:**
- Extension not installed
- Typo in extension ID
- Extension directory name differs from manifest ID

**Resolution:**
1. List installed extensions: `homeboy extension list`
2. Install missing extension: `homeboy extension install <url>`
3. Verify extension ID spelling

### Configuration Not Found

**Error:** `Project not found: <project_id>`

**Common causes:**
- Project not created
- Typo in project ID
- Configuration file deleted

**Resolution:**
1. List existing projects: `homeboy project list`
2. Create missing project: `homeboy project create`
3. Verify project ID spelling

### Permission Denied

**Error:** `Permission denied: <path>`

**Common causes:**
- File permissions too restrictive
- SSH key permissions incorrect
- Directory not writable

**Resolution:**
```bash
# Fix file permissions
chmod 600 ~/.config/homeboy/components/*.json

# Fix SSH key permissions
chmod 600 ~/.ssh/id_ed25519

# Fix directory permissions
chmod 700 ~/.config/homeboy
```

## Debugging Errors

### Verbose Mode

Enable verbose output for detailed information:

```bash
homeboy deploy mysite --verbose
```

### Dry Run Mode

Preview operations without executing:

```bash
homeboy deploy mysite --dry-run
```

### Check Command

Run checks before operations:

```bash
homeboy deploy mysite --check
```

### Review Logs

Check Homeboy logs for detailed error information:
- Location varies by platform
- Contains error stack traces

## Related

- [Architecture overview](./architecture-overview.md) - System architecture
- [Config directory structure](./config-directory.md) - File organization
