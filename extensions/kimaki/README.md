# Kimaki Extension

Homeboy extension for **Kimaki diagnostics and reproducibility**.

This is intentionally **not** just a thin wrapper around `kimaki`. The useful part is the script layer for:

- **doctor** — verify runtime, DB, logs, and config paths
- **inspect-models** — compare Kimaki DB model state against OpenCode fallback config
- **repro-model-fallback** — create a compact report for model drift / fallback debugging

## Example usage

```bash
homeboy kimaki chubes.net doctor
homeboy kimaki chubes.net inspect-models --recent 5
homeboy kimaki chubes.net repro-model-fallback --thread 1480783248375287849
homeboy kimaki chubes.net raw session list --json
```

## Current focus

The first regression target is the frustrating case where:

```text
Kimaki says model = GPT-5.4
but runtime falls back to Anthropic / Opus
until restart or explicit session override
```

This extension makes that class of bug easier to inspect repeatedly.
