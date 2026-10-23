# Pi response identity fixture

`pi-roadie-session.jsonl` contains the header and first two assistant message
records from a real Pi 1.4.0 Cook session on 2026-10-23. The requested route was
`cook-heavy`; the provider returned `gpt-6.1-sol` in the response model metadata.
The cwd is sanitized and content is replaced with a deliberately false model
self-report. Response metadata, usage and message framing are preserved.

Pi's Anthropic adapter maps `message_start.message.model` to
`AssistantMessage.responseModel` when it differs from the requested model.
Pi persists that field in session `message` records and emits it on
`message_end` / `turn_end`. Homeboy's Pi adapter supplies
`--session <artifacts>/<safe-task-id>-pi-session.jsonl`; Homeboy reads that exact
file, not arbitrary artifacts or structured agent outputs.

`message.model` and the adapter's legacy `metadata.model` can both be routing
aliases. Without `responseModel`, Homeboy conservatively reports that no served
model was recorded rather than asserting the route was the answering model.
