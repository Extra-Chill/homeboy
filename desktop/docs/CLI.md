# Homeboy CLI Reference

Command-line interface for Homeboy, providing terminal access to project management, remote CLI operations (WP-CLI, PM2), database queries, and deployments.

## Installation

The CLI binary is bundled inside the Homeboy app at `Homeboy.app/Contents/MacOS/homeboy-cli`.

**First Launch**: Homeboy prompts to install the CLI on first launch. This creates a symlink at `/usr/local/bin/homeboy`.

**Manual Installation**: Go to **Settings > General** and click **Install CLI**.

**Verify Installation**:
```bash
homeboy --version
# homeboy 0.6.0
```

## Configuration

The CLI uses the same configuration as the GUI app stored at:
```
~/Library/Application Support/Homeboy/
├── config.json           # Active project ID
├── projects/             # Project configurations
└── servers/              # SSH server configurations
```

Projects and servers can be configured via Homeboy.app or via the CLI (`project` and `server` commands).

## Commands

### projects

List configured projects.

```bash
homeboy projects
```

**Flags**:
- `--current` - Show only the active project ID

**Examples**:
```bash
# List all projects
homeboy projects
# extrachill (active)
# client-site

# Get active project ID (useful for scripts)
homeboy projects --current
# extrachill
```

### project

Manage project configurations.

#### project create

Create a new project.

```bash
homeboy project create <name> --type <type> [--id <id>]
```

**Arguments**:
- `name` - Project name (required)

**Options**:
- `--type` - Project type (required, e.g., `wordpress`, `nodejs`)
- `--id` - Project ID (default: auto-generated from name)

**Examples**:
```bash
# Create a WordPress project
homeboy project create "My Site" --type wordpress

# Create with custom ID
homeboy project create "Client Site" --type wordpress --id client-prod

# Create a Node.js project
homeboy project create "API Server" --type nodejs
```

#### project show

Show project configuration.

```bash
homeboy project show <id> [--field <field>]
```

**Arguments**:
- `id` - Project ID (required)

**Options**:
- `--field` - Specific field to show (supports dot notation)

**Examples**:
```bash
# Show full project config
homeboy project show extrachill

# Get specific field
homeboy project show extrachill --field domain

# Get nested field (dot notation)
homeboy project show extrachill --field database.name
```

#### project set

Update project configuration fields.

```bash
homeboy project set <id> [options]
```

**Arguments**:
- `id` - Project ID (required)

**Options**:
- `--domain` - Project domain
- `--server` - Server ID to link
- `--basePath` - Remote base path
- `--tablePrefix` - Database table prefix
- `--type` - Project type
- `--dbName` - Database name
- `--dbUser` - Database user
- `--dbHost` - Database host
- `--dbPort` - Database port
- `--apiEnabled` - Enable/disable API (true/false)
- `--apiUrl` - API base URL
- `--localWpCliPath` - Local WP-CLI path
- `--localDomain` - Local development domain

**Examples**:
```bash
# Set project domain
homeboy project set extrachill --domain extrachill.com

# Link to a server
homeboy project set extrachill --server production-1

# Configure database
homeboy project set extrachill --dbName wp_extrachill --dbUser admin
```

#### project delete

Delete a project.

```bash
homeboy project delete <id> --force
```

**Arguments**:
- `id` - Project ID (required)

**Flags**:
- `--force` - Confirm deletion (required)

**Notes**: Cannot delete the active project. Switch to another project first.

**Examples**:
```bash
homeboy project delete old-site --force
```

#### project switch

Switch the active project.

```bash
homeboy project switch <id>
```

**Arguments**:
- `id` - Project ID (required)

**Examples**:
```bash
homeboy project switch client-site
```

#### project subtarget

Manage project subtargets (e.g., WordPress multisite blogs or Node.js PM2 processes).

##### project subtarget add

```bash
homeboy project subtarget add <project> <id> --name <name> --domain <domain> [--number <n>] [--is-default]
```

**Arguments**:
- `project` - Project ID
- `id` - Subtarget ID (slug)

**Options**:
- `--name` - Display name (required)
- `--domain` - Domain (required)
- `--number` - Numeric ID (e.g., WordPress blog_id)
- `--is-default` - Set as default subtarget

**Examples**:
```bash
# Add a multisite blog
homeboy project subtarget add extrachill shop --name "Shop" --domain shop.extrachill.com --number 2
```

##### project subtarget remove

```bash
homeboy project subtarget remove <project> <id> --force
```

##### project subtarget list

```bash
homeboy project subtarget list <project>
```

Outputs JSON array of subtargets.

##### project subtarget set

```bash
homeboy project subtarget set <project> <id> [--name <name>] [--domain <domain>] [--number <n>] [--is-default]
```

#### project component

Manage project components (plugins, themes, packages).

##### project component add

```bash
homeboy project component add <project> <name> --local-path <path> --remote-path <path> --build-artifact <path> [--version-file <file>] [--version-pattern <regex>] [--group <group>]
```

**Arguments**:
- `project` - Project ID
- `name` - Component name

**Options**:
- `--local-path` - Local path to component source (required)
- `--remote-path` - Remote path relative to basePath (required)
- `--build-artifact` - Build artifact path relative to localPath (required)
- `--version-file` - Version file relative to localPath
- `--version-pattern` - Version regex pattern
- `--group` - Component group

**Examples**:
```bash
homeboy project component add extrachill my-plugin \
  --local-path ~/Developer/my-plugin \
  --remote-path plugins/my-plugin \
  --build-artifact build/my-plugin.zip \
  --version-file my-plugin.php \
  --version-pattern "Version:\\s*([0-9.]+)"
```

##### project component remove

```bash
homeboy project component remove <project> <id> --force
```

##### project component list

```bash
homeboy project component list <project>
```

Outputs JSON array of components.

### server

Manage server configurations.

#### server create

Create a new server configuration.

```bash
homeboy server create <name> --host <host> --user <user> [--port <port>]
```

**Arguments**:
- `name` - Server display name (required)

**Options**:
- `--host` - SSH host (required)
- `--user` - SSH username (required)
- `--port` - SSH port (default: 22)

**Notes**: SSH key must be configured in Homeboy.app after creating the server.

**Examples**:
```bash
homeboy server create "Production" --host server.example.com --user deploy
```

#### server show

Show server configuration.

```bash
homeboy server show <id>
```

#### server set

Update server configuration fields.

```bash
homeboy server set <id> [--name <name>] [--host <host>] [--user <user>] [--port <port>]
```

**Examples**:
```bash
homeboy server set production-1 --port 2222
```

#### server delete

Delete a server configuration.

```bash
homeboy server delete <id> --force
```

**Notes**: Fails if the server is used by any project. Update or delete the project first.

#### server list

List all server configurations.

```bash
homeboy server list
```

**Output**:
```json
{
  "servers": [
    {
      "id": "production-1",
      "name": "Production",
      "host": "server.example.com",
      "user": "deploy",
      "port": 22
    }
  ]
}
```

### wp

Execute WP-CLI commands on the production server (WordPress projects only).

```bash
homeboy wp <project> [subtarget] <command...>
```

**Arguments**:
- `project` - Project ID (required)
- `subtarget` - Subtarget ID for multisite (optional, case-insensitive)
- `command` - WP-CLI command and arguments

**Requirements**:
- Project type must be `wordpress`
- Server configured with SSH key
- Base path configured (wp-content path)

**Examples**:
```bash
# List plugins on production
homeboy wp extrachill plugin list

# Check WordPress version
homeboy wp extrachill core version

# Run command on specific multisite blog
homeboy wp extrachill shop plugin list

# Search/replace in database
homeboy wp extrachill search-replace 'old.com' 'new.com' --dry-run

# Export database
homeboy wp extrachill db export - > backup.sql
```

### pm2

Execute PM2 commands on remote Node.js servers (Node.js projects only).

```bash
homeboy pm2 <project> [subtarget] <command...>
```

**Arguments**:
- `project` - Project ID (required)
- `subtarget` - Subtarget ID (optional, case-insensitive)
- `command` - PM2 command and arguments

**Requirements**:
- Project type must be `nodejs`
- Server configured with SSH key
- Base path configured

**Examples**:
```bash
# List PM2 processes
homeboy pm2 api-server list

# Restart a process
homeboy pm2 api-server restart app

# View logs
homeboy pm2 api-server logs --lines 100

# Target specific subtarget
homeboy pm2 api-server staging restart app
```

### db

Database operations (read-only). Uses WP-CLI over SSH.

```bash
homeboy db <project> [subtarget] <subcommand>
```

#### db tables

List database tables.

```bash
homeboy db <project> tables [--json]
```

**Flags**:
- `--json` - Output as JSON instead of table

**Examples**:
```bash
# List tables
homeboy db extrachill tables

# List tables as JSON
homeboy db extrachill tables --json

# List tables for specific multisite blog
homeboy db extrachill shop tables
```

#### db describe

Show table structure.

```bash
homeboy db <project> describe <table> [--json]
```

**Arguments**:
- `table` - Table name to describe

**Flags**:
- `--json` - Output as JSON instead of table

**Examples**:
```bash
# Describe wp_posts table
homeboy db extrachill describe wp_posts

# Describe with JSON output
homeboy db extrachill describe wp_options --json
```

#### db query

Execute a SQL query (read-only).

```bash
homeboy db <project> query "<sql>" [--json]
```

**Arguments**:
- `sql` - SQL query (must be quoted)

**Flags**:
- `--json` - Output as JSON instead of table

**Read-Only Enforcement**: The following operations are blocked: INSERT, UPDATE, DELETE, DROP, ALTER, TRUNCATE, CREATE, REPLACE, GRANT, REVOKE. For write operations, use `homeboy wp <project> db query`.

**Examples**:
```bash
# Select users
homeboy db extrachill query "SELECT ID, user_login FROM wp_users LIMIT 10"

# Count posts by status
homeboy db extrachill query "SELECT post_status, COUNT(*) as count FROM wp_posts GROUP BY post_status"

# Query with JSON output
homeboy db extrachill query "SELECT * FROM wp_options WHERE option_name LIKE '%siteurl%'" --json

# Query specific multisite blog
homeboy db extrachill shop query "SELECT * FROM wp_2_posts LIMIT 5"
```

### deploy

Deploy plugins and themes to production.

```bash
homeboy deploy <project> [component-id...] [flags]
```

**Arguments**:
- `project` - Project ID (required)
- `component-id` - One or more component IDs to deploy (optional if using flags)

**Flags**:
- `--all` - Deploy all configured components
- `--outdated` - Deploy only components where local version differs from remote
- `--skip-missing` - Skip components not installed on the remote server
- `--dry-run` - Show what would be deployed without executing
- `--markdown` - Output as markdown instead of JSON

**Deployment Process**:
1. Execute `build.sh` in the component's local directory
2. Upload the generated zip file via SCP
3. Remove the old version on the remote server
4. Extract the new version
5. Set permissions (755)

**Requirements**:
- Components configured in project settings with local path and remote path
- Each component must have a `build.sh` script that creates a zip file
- Server configured with SSH key

**Examples**:
```bash
# Deploy specific components
homeboy deploy extrachill my-plugin my-theme

# Deploy all components
homeboy deploy extrachill --all

# Deploy only outdated components
homeboy deploy extrachill --outdated

# Preview deployment without executing
homeboy deploy extrachill --all --dry-run

# Deploy with markdown output
homeboy deploy extrachill my-plugin --markdown
```

**Output Format (JSON)**:
```json
{
  "success": true,
  "components": [
    {
      "id": "my-plugin",
      "name": "My Plugin",
      "status": "deployed",
      "duration": 12.5,
      "localVersion": "1.2.0",
      "remoteVersion": "1.1.0",
      "error": null
    }
  ],
  "summary": {
    "succeeded": 1,
    "failed": 0,
    "skipped": 0
  }
}
```

### ssh

SSH access to the project server.

```bash
homeboy ssh <project> [command]
```

**Arguments**:
- `project` - Project ID (required)
- `command` - Command to execute (optional)

**Behavior**:
- Without command: Opens an interactive SSH shell
- With command: Executes the command and returns

**Examples**:
```bash
# Open interactive shell
homeboy ssh extrachill

# Execute single command
homeboy ssh extrachill "ls -la /var/www"

# Check disk space
homeboy ssh extrachill "df -h"

# View recent logs
homeboy ssh extrachill "tail -50 ~/logs/error.log"
```

### module

Manage and run Homeboy modules from the command line. Only modules with `cli` runtime type can be executed via CLI.

#### module list

List available modules.

```bash
homeboy module list [--project <project>]
```

**Options**:
- `--project` - Filter by project compatibility

**Output**: Lists all installed modules with compatibility markers when `--project` is specified.

**Examples**:
```bash
# List all modules
homeboy module list

# List modules compatible with a project
homeboy module list --project extrachill
```

#### module run

Run a module with CLI runtime type.

```bash
homeboy module run <module-id> [--project <project>] [args...]
```

**Arguments**:
- `module-id` - Module ID (required)
- `args` - Additional arguments passed to the module

**Options**:
- `--project` - Project ID (defaults to active project)

**Requirements**:
- Module must have `cli` runtime type
- Project must have local CLI configured (`localCLI.sitePath`)
- Project type must support CLI (has `cli` configuration in type definition)

**Examples**:
```bash
# Run a module with active project
homeboy module run datamachine-scraper-tester --target_url https://venue.com/events

# Run a module with specific project
homeboy module run datamachine-scraper-tester --project extrachill --target_url https://venue.com/events

# Pass additional arguments
homeboy module run my-module arg1 arg2 --flag value
```

## Subtarget Support

Commands that accept a `[subtarget]` argument allow targeting specific subtargets within a project. For WordPress projects, subtargets represent multisite blogs. For Node.js projects, subtargets can represent different PM2 processes or environments.

**Subtarget Resolution**:
- Subtarget IDs are matched case-insensitively
- If no subtarget is provided or the ID doesn't match, the main project domain is used

**Configuration**: Subtargets are managed via `homeboy project subtarget` commands or in Homeboy.app.

**Example Subtargets Configuration**:
```json
[
  {
    "id": "main",
    "name": "Main Site",
    "domain": "example.com",
    "number": 1,
    "isDefault": true
  },
  {
    "id": "shop",
    "name": "Shop",
    "domain": "shop.example.com",
    "number": 2,
    "isDefault": false
  }
]
```

**Usage**:
```bash
# Target main site (default)
homeboy wp extrachill plugin list

# Target shop subtarget
homeboy wp extrachill shop plugin list

# Case-insensitive matching
homeboy wp extrachill SHOP plugin list
```

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error (configuration missing, command failed, etc.) |

## Error Messages

Common errors and solutions:

| Error | Solution |
|-------|----------|
| Project 'x' not found | Create the project with `homeboy project create` or configure in Homeboy.app |
| Server not configured | Link a server with `homeboy project set <id> --server <server-id>` |
| Server 'x' not found | Create the server with `homeboy server create` |
| SSH key not found | Generate SSH key in Homeboy.app Settings > Servers |
| Project type does not support remote CLI | Use a project type that supports CLI (wordpress, nodejs) |
| No components configured | Add components with `homeboy project component add` |
| Write operations not allowed | Use `homeboy wp <project> db query` for writes |
| Cannot delete active project | Switch to another project first with `homeboy project switch` |
| Server is used by project | Update or delete the project before deleting the server |
| Module 'x' not found | Use `homeboy module list` to see available modules |
| Module has runtime type 'x' which is not supported by CLI | Only modules with `cli` runtime type can be run from CLI |
| Local CLI not configured for project | Configure `Local Site Path` in Homeboy.app Settings |
| Project type does not support CLI | Use a project type with CLI configuration (wordpress, nodejs) |
