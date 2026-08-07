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

Kimaki-backed shell commands expose the owning Discord thread as
`KIMAKI_THREAD_ID`. The extension validates that invocation-scoped value and
derives `discord:v1:thread:<thread-id>` without notification flags. Homeboy
persists the opaque route with the run, so concurrent detached runs deliver
independently. Missing Kimaki context preserves route-less behavior; invalid
context fails closed.

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
