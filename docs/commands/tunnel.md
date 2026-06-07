# `homeboy tunnel`

## Synopsis

```sh
homeboy tunnel service <COMMAND>
```

`tunnel` manages Homeboy-native private service tunnel declarations and local managed service lifecycle. Homeboy can start a long-running local command, record safe command/process/log evidence, report readiness, and stop the process group without relying on an external chat or tunnel wrapper.

## Service Tunnels

Declare a private service reachable from a configured SSH server:

```sh
homeboy tunnel service expose context-a8c \
  --server wp-cloud-runtime \
  --remote-host 127.0.0.1 \
  --remote-port 7331 \
  --auth-mode bearer-env \
  --auth-env CONTEXTA8C_TOKEN \
  --auth-header Authorization \
  --allow-client wp-runtime
```

The declaration requires an explicit auth mode and stores a private-loopback policy. It does not expose a public unauthenticated URL.

Start a declared local service command and record lifecycle evidence:

```sh
homeboy tunnel service start context-a8c \
  --command 'npm run dev -- --host 127.0.0.1 --port 7331' \
  --cwd /path/to/workspace \
  --host 127.0.0.1 \
  --port 7331 \
  --health-path / \
  --public-tunnel-backend none
```

`--env KEY=VALUE` can be repeated to pass runtime environment values. Status records only env var names, not values. The managed service writes stdout/stderr logs under Homeboy's local data directory and includes those paths in `service status` output.

Public tunnel backends are an explicit seam. `none` is the only implemented backend in this release; unsupported backends should be added as first-class implementations rather than faked by wrapping Kimaki or another CLI in proof paths.

## Subcommands

- `service expose`: create or replace a private service tunnel declaration.
- `service list`: list declarations.
- `service show <id>`: show one declaration.
- `service set <id> ...`: update fields using the standard dynamic set contract.
- `service remove <id>`: delete a declaration.
- `service url <id>`: print the declared loopback URL.
- `service start <id>`: start and supervise a declared local service command.
- `service status <id>`: report declaration, process, local URL, public URL when present, health, backend, and log evidence state.
- `service stop <id>`: terminate the managed process group and remove runtime state while leaving log evidence files in place.
