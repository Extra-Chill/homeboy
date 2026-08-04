<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
cargo run -p homeboy-cli --bin generate-cli-reference
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy observe` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/observe.md](../../../commands/observe.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy observe`

```sh
homeboy observe [OPTIONS] [COMPONENT]
```

Passively observe a running system and persist timeline evidence

| Argument | Required | Description |
| --- | --- | --- |
| `[COMPONENT]` | no | Component ID (optional — auto-detected from CWD if omitted) |

| Option | Value | Description |
| --- | --- | --- |
| `--path` | `<PATH>` | Override the component checkout path for this invocation |
| `--duration` | `<DURATION>` | How long to observe before closing the run, as a duration such as `30s` or `5m` |
| `--tail-log` | `<PATH>` | Log file to tail for the length of the run. Repeatable |
| `--grep` | `<REGEX>` | Regex applied to every `--tail-log` probe, so only matching lines are recorded |
| `--watch-process` | `<REGEX>` | Regex matched against running process command lines; a snapshot is recorded on each interval. Repeatable |
| `--watch-process-interval` | `<WATCH_PROCESS_INTERVAL>` | How often `--watch-process` samples, as a duration such as `1s` |
| `--probe` | `<JSON>` | Raw `TraceProbeConfig` JSON for probes that the flags above cannot express. Repeatable |
