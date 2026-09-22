# OpenCode Agent Runtime

`opencode` is a generic repository-scoped agent-task runtime for Homeboy. It runs
the OpenCode CLI behind the `homeboy/agent-task-request/v1` and
`homeboy/agent-task-outcome/v1` contract, so callers can select OpenCode without
embedding CLI details in their own manifests.

## Runtime Contract

`opencode.json` declares the runtime manifest and executor provider contract:

- `command` and `invocation` point at the runtime-local executor wrapper.
- `runner_readiness` advertises the OpenCode executable check and install hint.
- `workspace_tools` declares the default repository workspace tool ids.
- `provider_defaults.openai` declares the optional scoped OpenAI API-key route;
  `provider_defaults.codex` declares Codex OAuth secret env names and source
  metadata.
- `provider_defaults.openai-oauth` declares the OpenCode auth-store handoff
  route as an additive opt-in: selecting `provider: "openai-oauth"` requires
  the `AI_PROVIDER_OPENCODE_OPENAI_ACCESS`, `AI_PROVIDER_OPENCODE_OPENAI_REFRESH`,
  and `AI_PROVIDER_OPENCODE_OPENAI_EXPIRES` secret env names, sourced through
  `json-file` fields from `~/.local/share/opencode/auth.json`. Homeboy core
  uploads that whole declared file to the same `~`-relative path on the runner
  before every agent-task run, and OpenCode reads it through its native store
  path. The env values are secondary; the file sync is the handoff. Exactly
  the selected route's credential names are required, so existing `openai`
  API-key configurations are unaffected and an OAuth-only setup never needs an
  API key.
- `provider_preflight` declares the auth checks callers should run before
  launching OpenCode.

### Auth-store handoff behavior

The controller's OpenCode auth store is the source of truth. The runner-side
copy is overwritten from the controller before every run and is never copied
back, so an OpenCode refresh on the runner cannot corrupt or diverge from the
controller's credentials: a rotated refresh token on the runner is discarded
with the next provisioning pass.

Sync targets the `~`-relative declared path (`~/.local/share/opencode/auth.json`).
In job environments where `XDG_DATA_HOME` is set, OpenCode resolves its store
under `$XDG_DATA_HOME` instead; the executor's process env allowlist forwards
only `HOME`, so agent-task execution resolves the synced store at the
provisioned `~` path. Readiness keeps `XDG_DATA_HOME` in its allowlist for
installations that genuinely relocate their store.

Core admission requires every required secret env name of the selected route
to resolve, and its `when` conditions can only inspect the serialized request —
not machine state such as whether the store currently holds an OAuth entry. A
single `openai` route therefore cannot conditionally switch between the API
key and the store; the `openai-oauth` account is the supported selector for
the store handoff until core grows a conditional source mechanism.

The JavaScript package exports the same provider contract through
`providerContract()`, plus `executeOpenCodeAgentTask()` for the CLI wrapper and
tests.

## Executor Behavior

The executor reads one AgentTaskRequest JSON object from stdin or
`HOMEBOY_AGENT_TASK_REQUEST`, validates `executor.backend: "opencode"`, then runs:

```sh
opencode run [--model <model>] [--agent <agent>] [--variant <variant>] [--title <title>] <instructions>
```

OpenCode's `--model` argument selects the run session model, but agent-local
configuration can still control built-in agents such as `build` and `title`.
For deterministic run-scoped selection, the executor also injects
`OPENCODE_CONFIG_CONTENT` with:

- `model` and `agent.build.model` from `executor.config.model`,
  `executor.model`, or top-level `model`.
- `small_model` from `executor.config.small_model` or
  `executor.config.smallModel` when provided.
- `agent.title.disable: true` for every agent-task run, after ambient config
  content is layered. Homeboy owns durable task, run, and pull request identity,
  so OpenCode session-title generation is not used. There is no title opt-in in
  this executor; a provider title failure therefore cannot affect coding work.

The title-disable overlay does not change the requested primary build model.
Ambient `agent.title` configuration may supply other fields, but cannot re-enable
title generation for an agent-task run.

Direct runtime verification can be done with a temporary config overlay, without
editing global OpenCode config:

```sh
OPENCODE_CONFIG_CONTENT='{"model":"opencode-go/kimi-k2.7-code","agent":{"build":{"model":"opencode-go/kimi-k2.7-code"}}}' opencode run --model opencode-go/kimi-k2.7-code 'Report the active provider/model for the build agent.'
```

The OpenCode binary is resolved from `executor.config.runtime_bin`,
`executor.config.command`, or `opencode` in that order. Additional leading
command args may be supplied with
`executor.config.command_args` or `HOMEBOY_OPENCODE_COMMAND_ARGS` as a JSON array.

The outcome includes status, diagnostics, and bounded metadata. It intentionally
does not include raw child stdout, stderr, argv, or secret environment values.

## Live Progress

While OpenCode runs, the executor translates its JSONL tool frames into
`homeboy/agent-task-progress/v1` envelopes. Each envelope has a stable per-run
`sequence` and `cursor`, a runtime-neutral activity type, and structured bounded
data. The executor writes them to `progress_events_path` (or its run artifact
directory) and also delivers them through the optional `onProgress` callback.

The adapter exposes tool, command, file, retry, and provider activity only. It
omits model messages and reasoning, redacts declared secret values and secret-like
command assignments, converts workspace paths to relative paths, and replaces all
other absolute paths with `<private-path>`. Repeated frames are coalesced and the
stream is capped at `max_progress_events` (default 200). The terminal result records
the same event summary and exposes `progress_events` when events were emitted.

## Usage Accounting

The adapter emits `metadata.provider_usage` from verified `step_finish` JSONL
frames. It sums only de-duplicated events identified by `sessionID` plus
`part.id`; missing fields, malformed lines, and scan truncation set the
corresponding `*_status` to `partial` or `unknown` and never become zero.
`total_tokens` is reported only when OpenCode supplies it, and `cost_usd: 0`
is known only when the event explicitly reports zero. Provider/model identity
comes from the separate session metadata path rather than invented JSONL keys.

This is the runtime-side producer for Homeboy #8282. Homeboy must ingest the JSONL
file or callback as its single canonical structured event stream, preserving the
cursor rather than serializing frames into log messages; that adoption is tracked by
Homeboy #9162.

## External Storage Retention

The retention adapter emits the strict five-key reclaim receipt: `schema`,
`provider_id`, `generation`, `reclaimed_item_ids`, and `reclaimed_bytes`. Native
event-log compaction is a bounded rewrite operation; logical payload savings are
private maintenance evidence and are never reported as physical reclaim. Physical
reclaim requires the native CLI's separate verified maintenance operation.

The native planner does not use an adapter-invented age threshold: default
selection is superseded, projection-verified local message/part snapshots, while
workspace- or sync-owned aggregates remain dry-run-only. A bounded response's
`next.cursor` and `next.afterSeq` are persisted as private evidence and consumed
on the next provider invocation after an interruption. They are not serialized as
one cursor or exposed in the Homeboy receipt.

Homeboy's external-storage planner defaults are `retention.external_storage_days`
of 7 and `retention.external_storage_max_bytes` of 20 GiB. Because compaction
inventory is intentionally reported with its real age and measured database
bytes, default unattended cleanup does not select a newly created large event
store. Operators may explicitly configure supported policy overrides; this does
not make physical vacuum automatic. The real planner check is
`npm run test:opencode-external-storage-homeboy-planner`.

Run the real SQLite/native dependency check with
`npm run test:opencode-external-storage-native-integration`. Set
`HOMEBOY_OPENCODE_NATIVE_ROOT` only when using a different settled native checkout.
The normal unit fixtures do not replace this dependency test.
