# Discord Notifications Extension

`discord` is an outbound-only Homeboy notification transport. It posts a run-completion message through Discord's REST API but does not own inbound interactions, buttons, or an orchestration lifecycle. Homeboy's durable run/daemon lifecycle remains authoritative.

Requires a Homeboy version with typed notification transport registry support.

## Setup

Install the extension and select exactly one authentication mode. Keep credentials in your shell secret manager or service environment; the helper never writes them or includes them in its JSON result.

```sh
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id discord

# Bot REST delivery. This optional channel is used only for route-less operations
# notifications; a run-scoped route always takes precedence.
export DISCORD_BOT_TOKEN='...'
export DISCORD_OPERATIONS_CHANNEL_ID='123456789012345678'

# Or webhook delivery to its configured default channel.
export DISCORD_WEBHOOK_URL='https://discord.com/api/webhooks/...'
```

Homeboy discovers the `discord.run-completion` transport from this extension's
manifest. A Homeboy version with extension-owned route resolver support can
select it automatically from invocation-scoped Discord context. Explicit
notification CLI or environment routes always take precedence.

## Usage

### Session context

A chat bridge that runs agent sessions describes the session that invoked
Homeboy through a bridge-neutral environment contract. The bridge (or its
installer) maps its own variables onto these names; this extension names no
bridge.

| Variable | Meaning |
| --- | --- |
| `HOMEBOY_SESSION_THREAD_ID` | Discord thread that owns the invoking session. |
| `HOMEBOY_SESSION_SEND_COMMAND` | Optional. Command that delivers a prompt into that session as a real turn. Split on whitespace and run without a shell as `<command...> --thread <thread-id> --prompt <text>`. |
| `HOMEBOY_SESSION_SEND_URL` | Optional, instead of the command. HTTP(S) endpoint that does the same. |
| `HOMEBOY_SESSION_SEND_TOKEN` / `HOMEBOY_SESSION_SEND_TOKEN_FILE` | Bearer token for the HTTP sender, inline or read from a file. |

The extension validates `HOMEBOY_SESSION_THREAD_ID` and derives
`discord:v1:thread:<thread-id>` without notification flags. Homeboy persists the
opaque route with the run, so concurrent detached runs deliver independently.
Missing session context preserves route-less behavior; invalid context fails
closed.

### Session delivery

A bridge usually ignores messages authored by its own bot, so a REST post to the
thread that owns a run never reaches the agent in that thread. When the route is
a thread, the status is an outcome (not `started`, `running`, or `queued`), and
the delivering host has a session sender configured, the notification is
delivered to the routed thread through the sender and becomes a turn the agent
can act on (`"mode":"session"`). Without a sender, for progress-only statuses,
and for channel routes, delivery uses the bot or webhook REST path below.

The route names the owning thread; ownership is not re-checked against the
delivering process's own session. A long-lived daemon, an outbox retry, or a
continuation started from another session delivers the routed thread all the
same.

The HTTP sender posts:

```json
{"options":{"thread":"<thread-id>","prompt":"<notification text>"}}
```

with `authorization: Bearer <token>`. Any 2xx response is delivered. The
response may stream NDJSON; a final `{"exit":<code>}` line with a non-zero code
is a delivery failure. `401`/`403` is reported as `auth_error`. The token, its
file path, and the endpoint never appear in results.

For example, a [Roadie](https://github.com/Extra-Chill/roadie) host exports:

```sh
export HOMEBOY_SESSION_THREAD_ID="$ROADIE_THREAD_ID"
export HOMEBOY_SESSION_SEND_COMMAND='roadie send'
# Or, from a different OS user, through the running bot's local send API:
export HOMEBOY_SESSION_SEND_URL='http://127.0.0.1:<port>/roadie/send'
export HOMEBOY_SESSION_SEND_TOKEN_FILE=/path/to/roadie-service-token
```

### Deprecated names

The bridge-specific names `KIMAKI_THREAD_ID`, `KIMAKI_CLI`, and
`KIMAKI_BOT_TOKEN` are still accepted for one release and will then be
removed. A host that sets `KIMAKI_THREAD_ID` or `KIMAKI_CLI` but no generic
sender delivers thread routes through `kimaki send` (or `$KIMAKI_CLI send`), as
before.
While any of them is in use, every result envelope carries a `deprecations`
list naming each one and its replacement:

```json
"deprecations":[{"name":"KIMAKI_THREAD_ID","replacement":"HOMEBOY_SESSION_THREAD_ID"}]
```

The generic names take precedence when both are set.

### Explicit routes

Explicit routes remain available for other callers. The canonical thread form
is `discord:v1:thread:<thread-id>`; legacy guild-bearing routes remain accepted
for existing persisted runs.

```sh
homeboy --notification-transport discord.run-completion \
  --notification-route 'discord:v1:thread:234567890123456789' \
  --detach-after-handoff test
```

Start the daemon after setting its service environment or shell:

```sh
homeboy daemon start
```

The helper emits one typed JSON envelope to stdout, for example:

```json
{"schema":"homeboy/discord-notification-result/v1","status":"delivered","delivery":{"mode":"bot","route_kind":"thread","destination":"dynamic_thread","content_length":74,"truncated":false},"attempts":1}
```

Use `--dry-run` to validate arguments and configuration shape without network I/O or secret output:

```sh
node ~/.config/homeboy/extensions/discord/scripts/notify.mjs \
  --run-id run-123 --status pass --title 'homeboy run pass' --body 'Run completed' --dry-run
```

Discord content is bounded to 2,000 characters. The helper retries a Discord `429` at most twice, using the service's `retry_after` value capped at five seconds. Authentication and destination/input rejections are reported as typed `auth_error` or `input_error` results; credentials, webhook URLs, and destination IDs are never included in diagnostics. Result evidence exposes only a route kind and a safe destination classification.

`DISCORD_OPERATIONS_CHANNEL_ID` is an optional bot-mode operations fallback only
when a run has no route. Without a route or this explicit fallback, bot delivery
fails closed. Webhooks use their configured default channel without a route, or
an explicit dynamic thread route; webhook delivery never reads ambient thread
configuration. Bot tokens remain service-level authentication and never appear
in routes.

`DISCORD_API_BASE_URL` is an optional HTTP(S) API base override for deterministic local testing only. Production bot delivery defaults to `https://discord.com/api/v10`.
