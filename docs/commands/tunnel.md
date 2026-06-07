# `homeboy tunnel`

## Synopsis

```sh
homeboy tunnel service <COMMAND>
```

`tunnel` manages Homeboy-native private service tunnel declarations and local managed service lifecycle. Homeboy can start a long-running local command, record safe command/process/log evidence, report readiness, and stop the process group without relying on an external chat or tunnel wrapper.

## Service Tunnels

Declare a private service reachable from a configured SSH server:

```sh
homeboy tunnel service expose site-preview \
  --server private-runtime \
  --remote-host 127.0.0.1 \
  --remote-port 7331 \
  --auth-mode bearer-env \
  --auth-env SITE_PREVIEW_TOKEN \
  --auth-header Authorization \
  --allow-client app-runtime \
  --preview-policy always
```

The declaration requires an explicit auth mode and stores a private-loopback policy. It does not expose a public unauthenticated URL. `--preview-policy` defaults to `none`; workloads can opt into `always`, `on-failure`, `manual-approval`, or `keep-alive-until` when reviewer-facing preview artifacts are useful.

Start a declared local service command and record lifecycle evidence:

```sh
homeboy tunnel service start site-preview \
  --command 'serve-app --host 127.0.0.1 --port 7331' \
  --cwd /workspace/app \
  --host 127.0.0.1 \
  --port 7331 \
  --health-path / \
  --public-tunnel-backend none \
  --source-run-id run-123 \
  --source-workflow-id workflow-abc
```

`--env KEY=VALUE` can be repeated to pass runtime environment values. Status records only env var names, not values. The managed service writes stdout/stderr logs under Homeboy's local data directory and includes those paths in `service status` output.

Public tunnel backends are an explicit seam. `none` is the only implemented backend in this release; unsupported backends should be added as first-class implementations rather than faked by wrapping Kimaki or another CLI in proof paths.

When a service's preview policy is relevant, `service status` and `service start` include a structured `preview` artifact with schema `homeboy/preview-url/v1`. The artifact records the service ID, local URL, optional public URL, backend, policy, cleanup/expiry metadata, and owning run/workflow IDs when the start command supplied them.

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
