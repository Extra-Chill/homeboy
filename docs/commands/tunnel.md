# `homeboy tunnel`

## Synopsis

```sh
homeboy tunnel service <COMMAND>
```

`tunnel` manages Homeboy-native private service tunnel declarations. The first wave records explicit service, auth, and policy configuration without opening public listeners or starting unauthenticated forwarding.

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

## Subcommands

- `service expose`: create or replace a private service tunnel declaration.
- `service list`: list declarations.
- `service show <id>`: show one declaration.
- `service set <id> ...`: update fields using the standard dynamic set contract.
- `service remove <id>`: delete a declaration.
- `service url <id>`: print the declared loopback URL.
- `service status <id>`: report declaration status. In this wave, status is intentionally no-op lifecycle state with `running: false`.
