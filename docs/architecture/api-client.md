# API Client System

The API client provides HTTP request capabilities with template-based authentication per project.

## Overview

Homeboy projects can configure an API client for making HTTP requests to project APIs. This supports:

- RESTful API interactions
- Template-based URL and header construction
- Keychain-stored authentication tokens
- JSON request/response handling
- Project-scoped `get`, `post`, `put`, `patch`, and `delete` requests

## Configuration

API client configuration lives in `projects/<project_id>.json`:

```json
{
  "api": {
    "base_url": "https://example.com/wp-json",
    "enabled": true
  }
}
```

### Configuration Fields

- **`base_url`** (string): Base URL for API requests
- **`enabled`** (boolean): Whether API client is active

## Authentication

Authentication credentials are stored securely in the OS keychain using `homeboy auth`.

Project auth variables choose their source in `api.auth.variables`:

```json
{
  "api": {
    "enabled": true,
    "base_url": "https://api.example.test/v1",
    "auth": {
      "header": "Auth: {{token}}",
      "variables": {
        "token": {
          "source": "keychain"
        }
      }
    }
  }
}
```

### Store Credentials

```bash
homeboy auth set --project <project_id> token
```

This prompts for the token and stores it in the keychain for the configured project variable.

### Keychain Storage

- **macOS**: Keychain Access (service: `homeboy`, account: `<project-id>:<variable-name>`)
- **Linux**: libsecret / gnome-keyring
- **Windows**: Windows Credential Manager

### Retrieved Credentials

Credentials are automatically retrieved from keychain when API requests are made. `source: "env"` remains the recommended path for CI and headless environments, and `source: "config"` remains available for non-secret values.

## Template Variables

API client templates support variables for constructing dynamic requests.

### Available Variables

- Header variables are the keys defined in `api.auth.variables`, such as `{{token}}`.

### Template Usage

Authentication header templates use `{{var}}` placeholders.

## API Commands

### Make Request

```bash
homeboy api <project_id> <command> <endpoint> [options]
```

**Arguments:**
- `<project_id>`: Project identifier
- `<command>`: Request command (`get`, `post`, `put`, `patch`, `delete`)
- `<endpoint>`: API endpoint path (appended to `base_url`)

**Options:**
- `--body <json>`: Request body for `post`, `put`, and `patch`
- `--form <key=value>`: Form field for `post`, `put`, and `patch` (repeatable)
- `--output <path>`: Write the structured JSON envelope to a file

**Examples:**

```bash
# GET request
homeboy api myproject get /posts

# POST with JSON body
homeboy api myproject post /posts --body '{"title": "Hello", "content": "World"}'

# POST with form fields
homeboy api myproject post /posts --form title=Hello --form status=draft

# Write structured response to a file
homeboy --output /tmp/posts.json api myproject get /posts
```

## Extension Integration

Extensions can define API actions in their manifest for automated API interactions.

### Extension Action API

```json
{
  "actions": [
    {
      "id": "sync_posts",
      "label": "Sync posts from API",
      "type": "api",
      "method": "GET",
      "endpoint": "/posts",
      "payload": {"per_page": 100}
    }
  ]
}
```

### Template Variables in Extension Actions

Extension API actions can use template variables:

```json
{
  "method": "POST",
  "endpoint": "/posts/{{postId}}/comments",
  "payload": {
    "content": "{{payload.comment}}"
  }
}
```

## Response Handling

### Success Responses

Successful API requests return the response body. Content type is respected:
- JSON responses are formatted and returned
- Text responses are returned as-is
- Binary responses can be saved to file via `--output`

### Error Responses

Failed requests return error information:
- HTTP status code
- Error message (if provided by API)
- Request details for debugging

### JSON Output

All API commands return responses wrapped in the global JSON envelope:

```json
{
  "success": true,
  "data": {
    "command": "api.request",
    "method": "GET",
    "endpoint": "/posts",
    "status_code": 200,
    "response": {
      "posts": []
    }
  }
}
```

## Use Cases

### WordPress REST API

```json
{
  "api": {
    "base_url": "https://example.com/wp-json",
    "enabled": true
  }
}
```

```bash
# List posts
homeboy api myproject get /wp/v2/posts

# Create post
homeboy api myproject post /wp/v2/posts --body '{"title": "New Post"}'
```

### Custom API

```json
{
  "api": {
    "base_url": "https://api.example.com/v1",
    "enabled": true
  }
}
```

```bash
# Make authenticated request
homeboy api myproject get /users
```

## Security Considerations

1. **Never store tokens in project JSON**: Use `source: "keychain"` locally and `homeboy auth set`, or use `source: "env"` in CI/headless environments
2. **Use HTTPS**: API base URLs should use HTTPS for secure communication
3. **Keychain storage**: Tokens are encrypted in OS keychain
4. **Token rotation**: Use `homeboy auth` to update tokens when they change

## Related

- [Auth command](../commands/auth.md) - Manage API authentication
- [API command](../commands/api.md) - Make API requests
- [Project schema](../schemas/project-schema.md) - API configuration structure
- [Keychain/secrets management](./keychain-secrets.md) - How secrets are stored
