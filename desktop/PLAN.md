# Extra Chill Desktop - Implementation Plan

## Overview

**Project**: `extrachill-desktop` - Native macOS SwiftUI application  
**Purpose**: Admin-only hub for grey hat automations and local WordPress development tools  
**Repository**: Private (GitHub)  
**Target**: macOS 14+ (Sonoma), Apple Silicon + Intel

## Modules

### 1. Bandcamp Scraper
Scrape artist emails from Bandcamp discover pages, subscribe to Sendy newsletter lists.
- Enter Bandcamp tag (e.g., "south-carolina", "lo-fi", "punk")
- Configure scroll depth (how many "View more" clicks)
- Real-time console output during scrape
- Results table with email, artist name, bio notes
- Checkbox selection for bulk newsletter subscription
- Direct Sendy list ID input (stored in UserDefaults)

### 2. WP-CLI Terminal
Execute WP-CLI commands on testing-grounds.local with real-time output.
- Pre-configured path: `/Users/chubes/Developer/LocalWP/testing-grounds/app/public`
- Command input with history (up/down arrows)
- Real-time streaming output
- Saved command shortcuts for frequent operations
- Integration point for coding agents (future: local HTTP API)

### 3. Settings
- WordPress authentication (login/logout)
- Sendy list ID configuration
- Local WP path configuration
- VPS URL configuration (future)

### 4. VPS Dashboard (Future - Placeholder)
- Instagram Bot management (when VPS deployed)
- Streaming controls (when VPS deployed)
- Disabled sidebar items until VPS is ready

---

## Architecture

```
extrachill-desktop/
├── ExtraChillDesktop.xcodeproj/
├── ExtraChillDesktop/
│   ├── App/
│   │   ├── ExtraChillDesktopApp.swift      # App entry point
│   │   └── ContentView.swift               # Root view with sidebar navigation
│   │
│   ├── Core/
│   │   ├── API/
│   │   │   ├── APIClient.swift             # HTTP client with JWT auth
│   │   │   └── APITypes.swift              # Response types (Codable)
│   │   ├── Auth/
│   │   │   ├── AuthManager.swift           # ObservableObject auth state
│   │   │   └── KeychainService.swift       # Secure token storage
│   │   ├── Process/
│   │   │   ├── PythonRunner.swift          # Python subprocess execution
│   │   │   └── ShellRunner.swift           # General shell commands (wp-cli)
│   │   └── Settings/
│   │       └── AppSettings.swift           # UserDefaults wrapper
│   │
│   ├── Modules/
│   │   ├── BandcampScraper/
│   │   │   ├── Views/
│   │   │   │   ├── BandcampScraperView.swift
│   │   │   │   └── ScraperResultsTable.swift
│   │   │   ├── BandcampScraperViewModel.swift
│   │   │   └── BandcampTypes.swift
│   │   │
│   │   └── WPCLITerminal/
│   │       ├── Views/
│   │       │   └── WPCLITerminalView.swift
│   │       └── WPCLITerminalViewModel.swift
│   │
│   ├── Views/
│   │   ├── LoginView.swift
│   │   ├── SettingsView.swift
│   │   └── SidebarView.swift
│   │
│   └── Resources/
│       ├── Assets.xcassets/
│       └── Scripts/
│           └── bandcamp_scraper.py         # Playwright-based scraper
│
├── venv/                                    # Python virtual environment
│   └── (managed by setup script)
│
├── ExtraChillDesktopTests/
├── AGENTS.md
├── PLAN.md                                  # This file
├── README.md
├── setup.sh                                 # First-run setup script
└── .gitignore
```

---

## Technology Stack

| Component | Technology | Purpose |
|-----------|------------|---------|
| **App Framework** | SwiftUI | Native macOS UI |
| **Networking** | URLSession | HTTP requests to WordPress API |
| **Auth Storage** | macOS Keychain | Secure token persistence |
| **Settings** | UserDefaults | Non-sensitive preferences |
| **Browser Automation** | Playwright (Python) | Bandcamp scraping |
| **Process Execution** | Foundation.Process | Python/shell subprocess |
| **WordPress CLI** | wp-cli | Local site management |

---

## Dependencies

### macOS/Swift (no external packages)
- SwiftUI (native)
- Foundation (URLSession, Process)
- Security.framework (Keychain)

### Python (via venv)
- playwright
- beautifulsoup4
- lxml
- requests
- tldextract

### System
- Python 3.12+ (Homebrew)
- wp-cli (Homebrew)
- Local WP (testing-grounds.local)

---

## Phase 1: Project Bootstrap

**Goal**: Create Xcode project with basic structure and sidebar navigation

### Tasks
1. Create Xcode project (SwiftUI macOS App)
2. Set up directory structure as shown above
3. Implement NavigationSplitView with sidebar
4. Create placeholder views for each module
5. Create AGENTS.md and .gitignore

### Files to Create
- `ExtraChillDesktop/App/ExtraChillDesktopApp.swift`
- `ExtraChillDesktop/App/ContentView.swift`
- `ExtraChillDesktop/Views/SidebarView.swift`
- `AGENTS.md`
- `.gitignore`

---

## Phase 2: Authentication System

**Goal**: Implement login flow matching mobile app pattern

### API Endpoints
```
POST /auth/login
  Body: { identifier, password, device_id }
  Response: { access_token, refresh_token, access_expires_at, user }

POST /auth/refresh
  Body: { refresh_token, device_id }
  Response: { access_token, refresh_token, access_expires_at }

GET /auth/me
  Headers: Authorization: Bearer <token>
  Response: { id, username, email, display_name, ... }

POST /auth/logout
  Body: { device_id }
```

### Tasks
1. **KeychainService**: Store/retrieve tokens securely
2. **APIClient**: HTTP client with auto token refresh
3. **AuthManager**: Observable auth state
4. **LoginView**: Username/password form
5. **Root view logic**: Show LoginView or ContentView based on auth state

### Files to Create
- `ExtraChillDesktop/Core/Auth/KeychainService.swift`
- `ExtraChillDesktop/Core/Auth/AuthManager.swift`
- `ExtraChillDesktop/Core/API/APIClient.swift`
- `ExtraChillDesktop/Core/API/APITypes.swift`
- `ExtraChillDesktop/Views/LoginView.swift`

---

## Phase 3: Python Environment & Scraper Rewrite

**Goal**: Set up Python venv with Playwright, rewrite scraper

### Playwright Benefits
- Bundles own browsers (no chromedriver)
- Better JS rendering handling
- Built-in stealth capabilities
- Consistent with VPS instagram-bot architecture

### Scraper CLI Interface
```bash
# Activate venv and run
source venv/bin/activate
python bandcamp_scraper.py --tag "south-carolina" --clicks 3 --output json
```

### CLI Arguments
```
--tag <string>        # Bandcamp tag to scrape (default: "")
--clicks <int>        # Number of "View more" clicks (default: 3)
--output <format>     # "json" or "csv" (default: "json")
--headless <bool>     # Run headless (default: true)
```

### JSON Output Format
```json
{
  "success": true,
  "tag": "south-carolina",
  "total_albums_scraped": 45,
  "results": [
    {
      "email": "band@example.com",
      "name": "Artist Name",
      "notes": "Bio text...",
      "source_url": "https://artistname.bandcamp.com/album/..."
    }
  ],
  "errors": []
}
```

### Output Streams
- **stderr**: Real-time progress/logging (streamed to Swift console)
- **stdout**: Final JSON result (parsed by Swift)

### Tasks
1. Create setup.sh for venv initialization
2. Rewrite bandcamp_scraper.py with Playwright
3. Add argparse CLI argument handling
4. Implement JSON output mode
5. Test scraper standalone

### Files to Create/Modify
- `setup.sh`
- `ExtraChillDesktop/Resources/Scripts/bandcamp_scraper.py`
- `requirements.txt`

---

## Phase 4: Bandcamp Scraper Module

**Goal**: SwiftUI interface for running scraper and viewing results

### UI Layout
```
┌─────────────────────────────────────────────────────────┐
│  Bandcamp Scraper                                       │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  Tag: [south-carolina________]                          │
│  View More Clicks: [3] (stepper)                        │
│                                                         │
│  [Start Scrape]                    Status: Ready        │
│                                                         │
│  ─────────────────────────────────────────────────────  │
│  Console Output                              [Collapse] │
│  ┌─────────────────────────────────────────────────┐   │
│  │ Discovering albums for tag 'south-carolina'...  │   │
│  │ Waiting for initial page load...                │   │
│  │ Scraping initially visible albums...            │   │
│  │ Found 24 initial album URLs.                    │   │
│  │ ...                                             │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
│  ─────────────────────────────────────────────────────  │
│  Results (47 emails found)                              │
│  Sendy List ID: [abc123_________] [Add Selected]       │
│  ┌───┬────────────────────┬─────────────────┬───────┐  │
│  │ ☑ │ Email              │ Artist          │ Notes │  │
│  ├───┼────────────────────┼─────────────────┼───────┤  │
│  │ ☑ │ band@gmail.com     │ The Band Name   │ Bio.. │  │
│  │ ☑ │ artist@site.com    │ Another Artist  │ Bio.. │  │
│  │ ☐ │ skip@example.com   │ Skip This One   │ Bio.. │  │
│  └───┴────────────────────┴─────────────────┴───────┘  │
│  [Select All] [Deselect All]                           │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

### Tasks
1. **PythonRunner**: Execute Python subprocess, stream stderr, capture stdout
2. **BandcampScraperView**: Main UI with inputs and results
3. **ScraperResultsTable**: Table with checkbox selection
4. **BandcampScraperViewModel**: State management and business logic
5. **BandcampTypes**: Codable structs for JSON parsing

### Files to Create
- `ExtraChillDesktop/Core/Process/PythonRunner.swift`
- `ExtraChillDesktop/Modules/BandcampScraper/Views/BandcampScraperView.swift`
- `ExtraChillDesktop/Modules/BandcampScraper/Views/ScraperResultsTable.swift`
- `ExtraChillDesktop/Modules/BandcampScraper/BandcampScraperViewModel.swift`
- `ExtraChillDesktop/Modules/BandcampScraper/BandcampTypes.swift`

---

## Phase 5: Newsletter Integration

**Goal**: Bulk subscribe scraped emails to Sendy via WordPress API

### New WordPress Endpoint
**File**: `extrachill-plugins/extrachill-api/inc/routes/admin/bulk-subscribe.php`

```
POST /extrachill/v1/admin/newsletter/bulk-subscribe
  Auth: Bearer token (requires manage_options capability)
  Body: {
    "emails": [
      { "email": "a@b.com", "name": "Artist Name" },
      { "email": "c@d.com", "name": "Another" }
    ],
    "list_id": "abc123xyz"
  }
  Response: {
    "success": true,
    "subscribed": 45,
    "already_subscribed": 3,
    "failed": 2,
    "errors": ["invalid@: Invalid email format"]
  }
```

### Tasks
1. Create bulk-subscribe.php API endpoint
2. Direct Sendy API integration (bypasses context system)
3. Add "Add Selected" button to scraper UI
4. Implement subscription flow with progress/results

### Files to Create
- `extrachill-plugins/extrachill-api/inc/routes/admin/bulk-subscribe.php`
- Update `BandcampScraperViewModel.swift` with `subscribeToNewsletter()` method

---

## Phase 6: WP-CLI Terminal Module

**Goal**: Execute WP-CLI commands on testing-grounds.local

### UI Layout
```
┌─────────────────────────────────────────────────────────┐
│  WP-CLI Terminal                                        │
│  Path: /Users/chubes/Developer/LocalWP/testing-grounds  │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │ $ wp core version                               │   │
│  │ 6.9                                             │   │
│  │                                                 │   │
│  │ $ wp plugin list --status=active               │   │
│  │ +------------------+--------+---------+        │   │
│  │ | name             | status | version |        │   │
│  │ +------------------+--------+---------+        │   │
│  │ | datamachine      | active | 1.2.0   |        │   │
│  │ | datamachine-ev...| active | 0.5.0   |        │   │
│  │ +------------------+--------+---------+        │   │
│  │                                                 │   │
│  │ $ _                                            │   │
│  └─────────────────────────────────────────────────┘   │
│                                                         │
│  Command: [wp ________________________________] [Run]   │
│                                                         │
│  Saved Commands:                                        │
│  [Clear Cache] [List Plugins] [DB Export] [+ Add]      │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

### Tasks
1. **ShellRunner**: Execute shell commands with streaming output
2. **WPCLITerminalView**: Terminal-style UI with command input
3. **WPCLITerminalViewModel**: Command history, saved commands
4. **Settings integration**: Configurable Local WP path

### Files to Create
- `ExtraChillDesktop/Core/Process/ShellRunner.swift`
- `ExtraChillDesktop/Modules/WPCLITerminal/Views/WPCLITerminalView.swift`
- `ExtraChillDesktop/Modules/WPCLITerminal/WPCLITerminalViewModel.swift`

---

## Phase 7: Polish & Documentation

**Goal**: Error handling, first-run experience, documentation

### Tasks
1. **First-run setup**
   - Check for Python, prompt to install venv
   - Check for Playwright browsers, prompt to install
   - Validate wp-cli path

2. **Error handling**
   - User-friendly error messages
   - Network retry logic
   - Graceful process termination

3. **Settings persistence**
   - Remember last used tag
   - Window position/size
   - Command history

4. **Documentation**
   - README.md with setup instructions
   - Update AGENTS.md with final structure

### Files to Create/Update
- `README.md`
- Update `AGENTS.md`
- `ExtraChillDesktop/Views/SettingsView.swift`

---

## Implementation Order

| Phase | Est. Time | Deliverable |
|-------|-----------|-------------|
| 1. Bootstrap | 1-2 hours | Xcode project with navigation |
| 2. Auth | 2-3 hours | Login flow, token management |
| 3. Python/Playwright | 2-3 hours | Rewritten scraper with CLI |
| 4. Scraper Module | 3-4 hours | Full scraper UI |
| 5. Newsletter | 2-3 hours | Bulk subscribe API + integration |
| 6. WP-CLI Terminal | 2-3 hours | Terminal UI and execution |
| 7. Polish | 1-2 hours | Error handling, docs |

**Total estimate**: 13-20 hours

---

## Future Enhancements

### VPS Integration (when deployed)
- Instagram Bot dashboard (account management, campaign config, real-time logs)
- Streaming controls (start/stop streams, view status)
- WebSocket connection for live updates

### Agent Integration
- Local HTTP API for WP-CLI execution
- Structured JSON responses for agent consumption
- Command suggestion based on context

### Additional Scrapers
- Other music platforms
- Event listing sites
- Social media profile discovery
