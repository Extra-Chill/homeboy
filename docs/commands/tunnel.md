# `homeboy tunnel`

## Synopsis

```sh
homeboy tunnel service <COMMAND>
```

`tunnel` manages Homeboy-native private service tunnel declarations and local managed service lifecycle. Homeboy can start a long-running local command, record safe command/process/log evidence, report readiness, and stop the process group without relying on an external chat or tunnel wrapper.

## Service Tunnels

Declare a private service reachable from a configured SSH server:

```sh
homeboy tunnel service expose hostname-app \
  --server private-runtime \
  --remote-host 127.0.0.1 \
  --remote-port 7331 \
  --auth-mode bearer-env \
  --auth-env HOSTNAME_APP_TOKEN \
  --auth-header Authorization \
  --allow-client browser-client
```

The declaration requires an explicit auth mode and stores a private-loopback policy. It does not expose a public unauthenticated URL.

Start a declared local service command and record lifecycle evidence:

```sh
homeboy tunnel service start hostname-app \
  --command 'npm run dev -- --host 127.0.0.1 --port 7331' \
  --cwd /path/to/workspace \
  --host 127.0.0.1 \
  --port 7331 \
  --health-path / \
  --public-tunnel-backend cloudflared \
  --declared-service-host app.example.test
```

`--env KEY=VALUE` can be repeated to pass runtime environment values. Status records only env var names, not values. The managed service writes stdout/stderr logs under Homeboy's local data directory and includes those paths in `service status` output.

Public tunnel backends are an explicit seam:

- `none`: start only the managed local service.
- `cloudflared`: start `cloudflared tunnel --url <local_bind_url>` and record the first HTTPS public origin printed by the provider.
- `command`: start an explicit `--public-tunnel-command` and record the first HTTPS public origin printed by the command. This is useful for deterministic tests and local provider adapters.

Persisted public URLs are reduced to safe origins (`scheme://host[:port]`) before writing state so tokenized paths, queries, and fragments are not stored. Raw provider stdout/stderr is written to local log files for operator inspection.

Status distinguishes the managed service bind URL, declared service host, expected Host header, public URL, effective browser origin, secure-context status, tunnel backend/session metadata, health/readiness status, and whether the generic Host-header probe reached the service.

## Subcommands

- `service expose`: create or replace a private service tunnel declaration.
- `service list`: list declarations.
- `service show <id>`: show one declaration.
- `service set <id> ...`: update fields using the standard dynamic set contract.
- `service remove <id>`: delete a declaration.
- `service url <id>`: print the declared loopback URL.
- `service start <id>`: start and supervise a declared local service command.
- `service status <id>`: report declaration, process, local URL, public URL when present, hostname/origin evidence, health, backend, and log evidence state.
- `service stop <id>`: terminate the managed service and public tunnel process groups, then remove runtime state while leaving log evidence files in place.
