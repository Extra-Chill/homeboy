# Copyable Error System

The Copyable system provides user-friendly error reporting with one-click copy to clipboard. When users encounter errors, they can copy structured markdown containing full debugging context for issue reporting.

## Core Types

### CopyableContent Protocol

The `CopyableContent` protocol defines content that can be copied as formatted markdown.

```swift
protocol CopyableContent {
    var title: String { get }
    var body: String { get }
    var context: ContentContext { get }
    var contentType: ContentType { get }
    var asMarkdown: String { get }
    
    func copyToClipboard()
}
```

**ContentType enum:**
- `.error` - Application errors
- `.warning` - Non-fatal warnings
- `.console` - Command output (deployment logs, WP-CLI output)
- `.report` - Structured reports
- `.info` - Informational content

The default implementation generates markdown with a context table containing timestamp, app version, project, server, and any additional info.

### ContentContext

Rich metadata for debugging context, populated from the active project and server configuration when available.

```swift
struct ContentContext {
    let source: String           // Module or component name
    let timestamp: Date
    let projectName: String?
    let serverName: String?
    let serverHost: String?
    let additionalInfo: [String: String]
}
```

**Factory methods:**

```swift
// Auto-populates from active project/server
ContentContext.current(source: "Database Browser")

// Include a file path in context
ContentContext.current(source: "Log Viewer", path: "/var/log/debug.log")

// Include custom additional info
ContentContext.current(source: "Deployer", additionalInfo: ["Component": "theme-starter"])
```

### AppError

Copyable error struct used throughout the application.

```swift
struct AppError: CopyableContent {
    let body: String
    let context: ContentContext
    
    var contentType: ContentType { .error }
}
```

**Initializers:**

```swift
// Basic error with source
AppError("Database credentials not configured", source: "Database Browser")

// Error with file path context
AppError("Failed to parse log file", source: "Log Viewer", path: "/var/log/debug.log")

// Error with additional info
AppError("Connection timeout", source: "SSH", additionalInfo: ["Host": "example.com"])

// Full control with custom context
AppError("Custom error", context: customContext)
```

### AppWarning

Same pattern as AppError for non-fatal warnings.

```swift
AppWarning("Server is running an outdated PHP version", source: "Health Check")
```

### ConsoleOutput

For command output and deployment logs.

```swift
ConsoleOutput(deploymentLog, source: "Deployer")
```

## View Components

### ErrorView

Full-screen error display for blocking errors. Includes copy button and optional retry action.

```swift
// With AppError
ErrorView(viewModel.error!, onRetry: { viewModel.retry() })

// Direct construction
ErrorView("Connection failed", source: "Database Browser", onRetry: retryAction)
```

### InlineErrorView

Compact inline error for forms and tight spaces. Red background, dismissible.

```swift
// With AppError
InlineErrorView(viewModel.error!, onDismiss: { viewModel.error = nil })

// Direct construction
InlineErrorView("Invalid URL format", source: "Module Runner")
```

### WarningView

Full-screen warning with optional action button.

```swift
WarningView("PHP version outdated", source: "Health Check", actionLabel: "Continue Anyway", onAction: proceed)
```

### InlineWarningView

Compact inline warning with orange background.

```swift
InlineWarningView("Large file may take time to load", source: "File Browser")
```

## CopyButton Component

Reusable copy button with visual feedback (checkmark animation on copy).

**Styles:**
- `.icon` - Icon only
- `.labeled` - Icon + "Copy"
- `.labeledError` - Icon + "Copy Error"
- `.labeledWarning` - Icon + "Copy Warning"
- `.labeledConsole` - Icon + "Copy Console"

**Convenience initializers:**

```swift
CopyButton.error("Failed to connect", source: "SSH")
CopyButton.warning("Deprecated API", source: "REST Client")
CopyButton.console(commandOutput, source: "WP-CLI Terminal")
```

## ViewModel Integration

All ViewModels follow a consistent pattern using `@Published var error: AppError?`:

```swift
class MyViewModel: ObservableObject {
    @Published var error: AppError?
    
    func performAction() async {
        do {
            try await riskyOperation()
        } catch {
            self.error = AppError(error.localizedDescription, source: "My Module")
        }
    }
}
```

**In Views:**

```swift
struct MyView: View {
    @StateObject var viewModel = MyViewModel()
    
    var body: some View {
        VStack {
            if let error = viewModel.error {
                InlineErrorView(error, onDismiss: { viewModel.error = nil })
            }
            // ... rest of view
        }
    }
}
```

## Markdown Output Format

When copied, errors produce structured markdown suitable for issue reporting:

```markdown
## Error: Database Browser

```
Database credentials not configured
```

### Context
| Field | Value |
|-------|-------|
| Timestamp | 2026-01-08T12:34:56Z |
| App | Homeboy <version> (Build <build>) |
| Project | Extra Chill |
| Server | Production (extrachill.com) |
| Path | `/var/log/debug.log` |
```

Path values are automatically wrapped in backticks for markdown formatting.

## Best Practices

1. **Provide meaningful source names** - Use the module or component name (e.g., "Database Browser", "Deployer", "SSH Tunnel")

2. **Include path context for file operations** - Use the `path:` parameter when errors relate to specific files

3. **Use AppError consistently** - All ViewModel errors should use `@Published var error: AppError?` for uniform UX

4. **Choose appropriate view components:**
   - `ErrorView` - Blocking errors requiring user action
   - `InlineErrorView` - Non-blocking errors in forms
   - `WarningView` - Warnings requiring acknowledgment
   - `InlineWarningView` - Non-blocking warnings

5. **Add relevant additional info** - Use `additionalInfo` dictionary for debugging-relevant context (component names, URLs, config values)
