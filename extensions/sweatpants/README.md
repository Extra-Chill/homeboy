# Sweatpants Module for Homeboy

Bridge to Sweatpants automation engine for long-running, checkpoint-resumable jobs.

## When to Use

| Use Case | Tool |
|----------|------|
| Quick local scripts (build/lint/test) | Homeboy subprocess |
| Interactive CLI (wp, cargo, swift) | Homeboy subprocess |
| Long-running automation (hours/days) | Sweatpants |
| Browser-based scraping | Sweatpants |
| Jobs needing checkpoint/resume | Sweatpants |
| Remote execution on VPS | Sweatpants |

## Installation

```bash
homeboy module install homeboy-modules/sweatpants
```

## Configuration

Endpoints are configured in `~/.config/homeboy/sweatpants/endpoints.json`:

```json
{
  "local": {
    "url": "http://127.0.0.1:8420",
    "auth": null
  },
  "vps": {
    "url": "https://sweatpants.myserver.com:8420",
    "auth": "your-api-token"
  }
}
```

## Commands

### Endpoint Management

```bash
# List configured endpoints
homeboy sweatpants endpoints

# Add new endpoint
homeboy sweatpants endpoint add <id> <url> [auth-token]

# Remove endpoint
homeboy sweatpants endpoint remove <id>
```

### Instance Commands

Commands can be prefixed with an endpoint ID, or use the default endpoint:

```bash
# Using default endpoint (local)
homeboy sweatpants status
homeboy sweatpants module list
homeboy sweatpants run <module> [-i key=value]...
homeboy sweatpants jobs
homeboy sweatpants logs <job-id> [--follow]
homeboy sweatpants cancel <job-id>

# Using explicit endpoint
homeboy sweatpants vps status
homeboy sweatpants vps run my-scraper -i query=test
```

## Examples

```bash
# Check local Sweatpants status
homeboy sweatpants status

# List available modules
homeboy sweatpants module list

# Run a module with inputs
homeboy sweatpants run my-scraper -i url=https://example.com -i depth=3

# Follow job logs in real-time
homeboy sweatpants logs abc123 --follow

# Run on remote VPS
homeboy sweatpants vps run my-scraper -i query=test
```

## Requirements

- `jq` for JSON parsing
- `curl` for API requests
- Running Sweatpants instance at configured endpoint
