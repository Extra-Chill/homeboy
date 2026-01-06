# Agent Instructions (extrachill-desktop)

## Project Overview

Native macOS SwiftUI application serving as an admin-only hub for grey hat automations and local WordPress development tools. Part of the Extra Chill Platform ecosystem.

**Platform**: macOS 14+ (Sonoma)
**Framework**: SwiftUI
**Architecture**: MVVM with ObservableObject ViewModels

## Commands

```bash
# Open in Xcode (after creating .xcodeproj)
open ExtraChillDesktop.xcodeproj

# Set up Python environment
./setup.sh

# Run Python scraper directly (for testing)
source venv/bin/activate
python ExtraChillDesktop/Resources/Scripts/bandcamp_scraper.py --tag "south-carolina" --clicks 3

# WP-CLI on local site
cd /Users/chubes/Developer/LocalWP/testing-grounds/app/public
wp core version
```

## Code Style

- **Swift**: SwiftUI with MVVM pattern
- **Naming**: PascalCase types, camelCase properties/functions
- **Async**: Use async/await for network operations
- **State**: @Published for reactive state, @AppStorage for UserDefaults
- **Formatting**: 4-space indent, Xcode defaults

## Directory Structure

```
ExtraChillDesktop/
├── App/                          # App entry and root view
├── Core/
│   ├── API/                      # HTTP client and types
│   ├── Auth/                     # Keychain and auth management
│   ├── Process/                  # Python and shell runners
│   └── Settings/                 # UserDefaults wrappers
├── Modules/
│   ├── BandcampScraper/          # Email scraping module
│   └── WPCLITerminal/            # WP-CLI execution module
├── Views/                        # Shared views (Login, Settings, Sidebar)
└── Resources/
    └── Scripts/                  # Python scripts (bundled)
```

## Modules

### Bandcamp Scraper
Scrapes Bandcamp discover pages for artist emails using Playwright.
- Input: Bandcamp tag, number of "View more" clicks
- Output: List of emails with artist name and bio
- Integration: Bulk subscribe to Sendy newsletter lists

### WP-CLI Terminal
Execute WP-CLI commands on testing-grounds.local.
- Real-time output streaming
- Command history (up/down arrows)
- Saved command shortcuts

## API Integration

**Base URL**: `https://extrachill.com/wp-json/extrachill/v1`

### Authentication
Uses JWT access/refresh token pattern (matches extrachill-app mobile).
- Tokens stored in macOS Keychain
- Auto-refresh before expiry (1 min buffer)
- Device ID stored in UserDefaults

### Endpoints
- `POST /auth/login` - Login with identifier/password
- `POST /auth/refresh` - Refresh access token
- `GET /auth/me` - Get current user
- `POST /auth/logout` - Logout
- `POST /admin/newsletter/bulk-subscribe` - Bulk subscribe emails (admin only)

## Python Environment

Uses a virtual environment for Playwright-based scraping.

**Location**: `./venv/`
**Python**: 3.12+ (Homebrew)

**Dependencies**:
- playwright
- beautifulsoup4
- lxml
- requests
- tldextract

**Setup**:
```bash
python3 -m venv venv
source venv/bin/activate
pip install playwright beautifulsoup4 lxml requests tldextract
python -m playwright install chromium
```

## Security

- Auth tokens in Keychain (kSecClassGenericPassword)
- Non-sensitive settings in UserDefaults
- No hardcoded secrets
- Admin-only functionality requires WordPress manage_options capability
