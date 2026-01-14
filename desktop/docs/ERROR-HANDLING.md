# Copyable Error System

The Copyable system standardizes errors/warnings/console output so users can copy a single markdown payload with relevant debugging context.

## Core types

- `CopyableContent` (`Homeboy/Core/Copyable/CopyableContent.swift`): protocol for anything copyable as markdown.
- `ContentContext` (`Homeboy/Core/Copyable/ContentContext.swift`): metadata attached to copy payloads (source, timestamp, optional project/server context, and additional info).
- `AppError` / `AppWarning` (`Homeboy/Core/Copyable/AppError.swift`, `Homeboy/Core/Copyable/AppWarning.swift`): app-native error/warning types.
- `CLIError` (`Homeboy/Core/Copyable/CLIError.swift`): structured CLI error matching the CLI JSON contract (code/message/details/hints/retryable).
- `DisplayableError` (`Homeboy/Core/Copyable/DisplayableError.swift`): common UI-facing protocol implemented by `AppError` and `CLIError`.
- `ConsoleOutput` (`Homeboy/Core/Copyable/ConsoleOutput.swift`): copyable command output.

## ViewModel pattern

ViewModels typically publish errors as `@Published var error: (any DisplayableError)?` so the UI can render either `AppError` or `CLIError` (both conform to `DisplayableError`).

```swift
@Published var error: (any DisplayableError)?

func performAction() async {
    do {
        try await riskyOperation()
    } catch {
        self.error = error.toDisplayableError(source: "My Module")
    }
}
```

## UI components

- `ErrorView` / `InlineErrorView`
- `WarningView` / `InlineWarningView`
- `CopyButton`

Views typically render an inline error when `viewModel.error` is non-nil.

```swift
if let error = viewModel.error {
    InlineErrorView(error, onDismiss: { viewModel.error = nil })
}
```

## Constructing errors

Prefer providing a stable `source` string (module/tool name) and include file context when available.

```swift
AppError("Database credentials not configured", source: "Database Browser")
AppError("Failed to parse log file", source: "Log Viewer", path: "/var/log/debug.log")
AppError("Connection timeout", source: "SSH", additionalInfo: ["Host": "example.com"])
```

## Copy format (high level)

Copied content is markdown that includes the message body plus a context table derived from `ContentContext`.
