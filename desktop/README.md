# Homeboy

A visual dashboard for all your web projects. See everything in one place—what's deployed, what needs updating, and what's happening across your sites.

Homeboy Desktop connects to your servers and shows you a unified view of all your projects, components, and deployments. No more switching between SSH terminals, FTP clients, and scattered scripts.

**Perfect for:** Site owners, project managers, and developers who prefer a visual interface over the command line.

**Powered by:** The [Homeboy CLI](../homeboy/) under the hood (required, installed separately).

## What You Get

### One Dashboard for Everything

- **See all your sites** — Know what's deployed, what's outdated, and what needs attention at a glance
- **One-click deployments** — Push updates without touching the terminal
- **Project overview** — Visual status across all your projects and components

### Built-in Tools

**Deployer** — Push code to any server with a click
- One-click deployment of plugins, themes, and packages
- Version comparison (see what's outdated before you deploy)
- Automatic backups before changes

**Database Browser** — Explore your databases visually
- Browse tables without writing SQL
- Run queries and see results in a clean table view
- Copy data to clipboard instantly

**Remote File Editor** — Edit files on your server directly
- Pinnable tabs for frequently accessed files
- Syntax highlighting for code files
- Automatic backups before saves

**Remote Log Viewer** — Monitor logs in real-time
- Live log streaming with filtering
- Search across log files
- Pinnable tabs for important logs

## Quick Start

1. **Install Homeboy CLI** first (the Desktop app needs it):
   ```bash
   brew tap Extra-Chill/homebrew-tap
   brew install homeboy
   ```

2. **Download Homeboy Desktop** from the [releases page](https://github.com/Extra-Chill/homeboy-desktop/releases)

3. **Set up your first server**:
   - Open Settings → Servers → Add Server
   - Enter your server details (host, username)
   - Generate an SSH key and add it to your server

4. **Add a project**:
   - Settings → Projects → Add Project
   - Link it to your server
   - Configure the path where your site lives on the server

5. **Start managing**:
   - View your dashboard
   - Deploy components with one click
   - Browse databases and logs visually

## System Requirements

- **macOS 14.4+** (Sonoma or later)
- **Homeboy CLI** must be installed ([see CLI installation](../homeboy/#installation))
- Optional: Python 3.12+ (only needed for certain modules)

> **Note:** The Desktop app may lag behind CLI features. The CLI is the source of truth for behavior.

## What Homeboy Desktop Is NOT

- **Not a hosting platform** — You bring your own servers (VPS, shared hosting, etc.)
- **Not a code editor** — Use it alongside your IDE, not instead of it
- **Not cross-platform** — macOS only; for Windows/Linux use the [CLI](../homeboy/)

## Documentation

- [CLI documentation](../homeboy/docs/) — Complete command reference
- [Desktop/CLI integration](docs/CLI.md) — How they work together
- [Module specification](docs/MODULE-SPEC.md) — Extend with custom modules

## License

GNU General Public License v2.0 or later. See [LICENSE](LICENSE).

Created by Chris Huber · https://chubes.net
