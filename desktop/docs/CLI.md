# Homeboy CLI Reference

Command-line interface for Homeboy, providing terminal access to project management, WordPress operations, database queries, and deployments.

## Installation

The CLI binary is bundled inside the Homeboy app at `Homeboy.app/Contents/MacOS/homeboy-cli`.

**First Launch**: Homeboy prompts to install the CLI on first launch. This creates a symlink at `/usr/local/bin/homeboy`.

**Manual Installation**: Go to **Settings > General** and click **Install CLI**.

**Verify Installation**:
```bash
homeboy --version
# homeboy 0.4.0
```

## Configuration

The CLI uses the same configuration as the GUI app stored at:
```
~/Library/Application Support/Homeboy/
├── config.json           # Active project ID
├── projects/             # Project configurations
└── servers/              # SSH server configurations
```

Projects and servers must be configured via Homeboy.app before using the CLI.

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

### wp

Execute WP-CLI commands on the production server.

```bash
homeboy wp <project> [blog-nickname] <command...>
```

**Arguments**:
- `project` - Project ID (required)
- `blog-nickname` - Multisite blog nickname (optional, case-insensitive)
- `command` - WP-CLI command and arguments

**Requirements**:
- Server configured with SSH key
- WordPress deployment configured (wp-content path)

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

### db

Database operations (read-only). Uses WP-CLI over SSH.

```bash
homeboy db <project> [blog-nickname] <subcommand>
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

## Multisite Support

For WordPress multisite installations, commands that accept a `[blog-nickname]` argument allow targeting specific blogs in the network.

**Blog Nickname Resolution**:
- Nicknames are matched case-insensitively against blog names configured in the project
- If no nickname is provided or the nickname doesn't match, the main site domain is used

**Configuration**: Blog nicknames are configured in Homeboy.app under **Settings > WordPress > Multisite**.

**Example Configuration**:
```json
{
  "multisite": {
    "enabled": true,
    "blogs": [
      { "name": "Main", "domain": "example.com" },
      { "name": "Shop", "domain": "shop.example.com" },
      { "name": "Blog", "domain": "blog.example.com" }
    ]
  }
}
```

**Usage**:
```bash
# Target main site (default)
homeboy wp extrachill plugin list

# Target shop blog
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
| Project 'x' not found | Configure the project in Homeboy.app |
| Server not configured | Add server in Settings > Servers |
| SSH key not found | Generate SSH key in Settings > Servers |
| WordPress deployment not configured | Set wp-content path in Settings > Servers |
| No components configured | Add components in Settings > Components |
| Write operations not allowed | Use `homeboy wp <project> db query` for writes |
