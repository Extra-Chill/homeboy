# Discord Notifications Extension

`discord` is an outbound-only Homeboy notification transport. It posts a run-completion message through Discord's REST API but does not own inbound interactions, buttons, or an orchestration lifecycle. Homeboy's durable run/daemon lifecycle remains authoritative.

Requires Homeboy `>=0.281.20`, which provides `HOMEBOY_NOTIFY_COMMAND` and `runs watch --notify`.

## Setup

Install the extension and select exactly one authentication mode. Keep credentials in your shell secret manager or service environment; the helper never writes them or includes them in its JSON result.

```sh
homeboy extension install https://github.com/Extra-Chill/homeboy-extensions --id discord

# Bot REST delivery to a channel.
export DISCORD_BOT_TOKEN='...'
export DISCORD_CHANNEL_ID='123456789012345678'

# Or bot REST delivery to a thread. Do not also set DISCORD_CHANNEL_ID.
export DISCORD_THREAD_ID='123456789012345678'

# Or webhook delivery. DISCORD_THREAD_ID is optional for webhook thread routing.
export DISCORD_WEBHOOK_URL='https://discord.com/api/webhooks/...'
```

Set `HOMEBOY_NOTIFY_COMMAND` to the installed helper. Placeholders are expanded by Homeboy as separate argv values, so titles and bodies containing spaces are supported.

```sh
export HOMEBOY_NOTIFY_COMMAND='node ~/.config/homeboy/extensions/discord/scripts/notify.mjs --run-id {run_id} --status {status} --title {title} --body {body}'
```

## Usage

Notify after a watched run settles:

```sh
homeboy runs watch run-123 --notify
```

A Homeboy daemon uses the same `HOMEBOY_NOTIFY_COMMAND` completion-notification seam. Start the daemon normally after setting the environment in its service definition or shell:

```sh
homeboy daemon start
```

The helper emits one typed JSON envelope to stdout, for example:

```json
{"schema":"homeboy/discord-notification-result/v1","status":"delivered","delivery":{"mode":"bot","destination":"thread","content_length":74,"truncated":false},"attempts":1}
```

Use `--dry-run` to validate arguments and configuration shape without network I/O or secret output:

```sh
node ~/.config/homeboy/extensions/discord/scripts/notify.mjs \
  --run-id run-123 --status pass --title 'homeboy run pass' --body 'Run completed' --dry-run
```

Discord content is bounded to 2,000 characters. The helper retries a Discord `429` at most twice, using the service's `retry_after` value capped at five seconds. Authentication and destination/input rejections are reported as typed `auth_error` or `input_error` results; credentials and webhook URLs are never included in diagnostics.

`DISCORD_API_BASE_URL` is an optional HTTP(S) API base override for deterministic local testing only. Production bot delivery defaults to `https://discord.com/api/v10`.
