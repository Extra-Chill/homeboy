# Extra Chill Desktop

Native macOS SwiftUI application for admin tools and WordPress deployment automation. Part of the [Extra Chill](https://extrachill.com) platform ecosystem.

## Features

### Bandcamp Scraper
Scrapes Bandcamp discover pages for artist contact emails using Playwright headless browser automation.
- Configure Bandcamp tag and scroll depth
- Extract emails, artist names, and bios
- Bulk subscribe results to Sendy newsletter lists via REST API

### Cloudways Deployer
One-click deployment of WordPress plugins and themes to a Cloudways server.
- SSH key authentication (RSA 4096-bit)
- Build automation via `build.sh` scripts
- Version comparison (local vs remote)
- Supports 24 components (1 theme, 21 plugins)

### WP-CLI Terminal
Execute WP-CLI commands on Local by Flywheel sites.
- Real-time output streaming
- Command history navigation
- Environment detection for Local by Flywheel PHP/MySQL paths

## Requirements

- macOS 14.0+ (Sonoma)
- Xcode 15.0+
- Python 3.12+ (Homebrew)
- Playwright with Chromium
- Homebrew packages: `composer`, `wp-cli` (for deployment and WP-CLI features)

## Setup

### 1. Clone and Open

```bash
git clone https://github.com/Extra-Chill/extrachill-desktop.git
cd extrachill-desktop
open ExtraChillDesktop.xcodeproj
```

### 2. Python Environment (for Bandcamp Scraper)

```bash
./setup.sh
```

This creates a virtual environment and installs:
- playwright
- beautifulsoup4
- lxml
- requests
- tldextract

### 3. Build and Run

Build and run from Xcode (Cmd+R). The app uses ad-hoc code signing.

## Configuration

All configuration is done in the app's Settings view:

- **Extra Chill Platform Path**: Local path to your Extra Chill Platform repository
- **Data Machine Path**: Local path to Data Machine Ecosystem repository
- **Cloudways Server**: Host, username, and application path for SSH deployment
- **SSH Key**: Generate RSA 4096-bit key pair (add public key to Cloudways)
- **Local WP Path**: Path to Local by Flywheel site for WP-CLI

## Project Structure

```
ExtraChillDesktop/
├── App/                          # App entry point
├── Core/
│   ├── API/                      # REST API client
│   ├── Auth/                     # Keychain and authentication
│   ├── Process/                  # Python and shell runners
│   └── SSH/                      # SSH/SCP operations
├── Modules/
│   ├── BandcampScraper/          # Email scraping module
│   ├── CloudwaysDeployer/        # Deployment module
│   └── WPCLITerminal/            # WP-CLI execution
├── Views/                        # Shared views
└── Resources/
    └── Scripts/                  # Python scripts
```

## Component Registry

The Cloudways Deployer is configured for the Extra Chill ecosystem:

**Theme**: extrachill

**Network Plugins**: multisite, users, ai-client, api, search, newsletter, admin-tools, analytics, seo

**Site Plugins**: blog, docs, shop, stream, artist-platform, community, events, news-wire, contact, chat, horoscopes, blocks-everywhere

**Data Machine**: data-machine, datamachine-events

To use with a different project, modify `DeployableComponent.swift` to define your own component registry.

## API Authentication

The app authenticates with the Extra Chill REST API using JWT tokens:
- Access and refresh tokens stored in macOS Keychain
- Auto-refresh before token expiry
- Requires WordPress user with `manage_options` capability

## License

MIT License - see [LICENSE](LICENSE) for details.

## Related

- [Extra Chill](https://extrachill.com) - Independent music platform
- [Extra Chill Mobile](https://github.com/Extra-Chill/extrachill-app) - React Native mobile app
