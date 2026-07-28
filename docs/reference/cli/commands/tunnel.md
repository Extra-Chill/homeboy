<!-- GENERATED FILE. DO NOT EDIT BY HAND.
Source of truth: the clap command tree in `crates/homeboy-cli`.
Regenerate with:
HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
Hand-written narrative for these commands lives in `docs/commands/`. -->

# `homeboy tunnel` command reference

Generated from the clap command tree. This page is the complete synopsis, argument, flag, and subcommand surface for this command family.

Concepts, recipes, and contracts are hand-written in [docs/commands/tunnel.md](../../../commands/tunnel.md).

Global flags apply to every command and are documented once in [the root command reference](../homeboy-root-command.md).

## `homeboy tunnel`

```sh
homeboy tunnel <COMMAND>
```

Manage private service tunnel declarations

| Subcommand | Summary |
| --- | --- |
| `homeboy tunnel service` | Manage private service tunnel declarations |
| `homeboy tunnel preview-client` | Connect a local preview origin to a Homeboy preview ingress |
| `homeboy tunnel preview-ingress` | Run and inspect the VPS-side public preview ingress |
| `homeboy tunnel preview-consumer` | Run a configured preview consumer with a Homeboy-owned public URL |
| `homeboy tunnel artifact-origin` | Serve the artifact root as a browser/reviewer-facing static origin |

## `homeboy tunnel service`

```sh
homeboy tunnel service <COMMAND>
```

Manage private service tunnel declarations

| Subcommand | Summary |
| --- | --- |
| `homeboy tunnel service expose` | Declare a private service tunnel without opening a public listener |
| `homeboy tunnel service list` | List private service tunnel declarations |
| `homeboy tunnel service show` | Show a private service tunnel declaration |
| `homeboy tunnel service set` | Modify a private service tunnel declaration |
| `homeboy tunnel service remove` | Remove a private service tunnel declaration |
| `homeboy tunnel service url` | Print the declared private local URL for a service tunnel |
| `homeboy tunnel service status` | Show declaration, process, health, backend, and evidence status |
| `homeboy tunnel service start` | Start and supervise a declared local service command |
| `homeboy tunnel service stop` | Stop a running managed local service and cleanup runtime state |

## `homeboy tunnel service expose`

```sh
homeboy tunnel service expose [OPTIONS] <ID>
```

Declare a private service tunnel without opening a public listener

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

| Option | Value | Description |
| --- | --- | --- |
| `--server` | `<SERVER>` | SSH server that can reach the private service |
| `--runner-local` | flag | Declare a runner-local service without a separate server declaration. In a runner-local context the runner itself is the server, so a duplicate server declaration is not required (#4606) |
| `--remote-host` | `<REMOTE_HOST>` | Hostname or IP of the service as seen from the SSH server |
| `--remote-port` | `<REMOTE_PORT>` | Port of the service as seen from the SSH server |
| `--scheme` | `<SCHEME>` | URL scheme for the local service URL |
| `--local-port` | `<LOCAL_PORT>` | Fixed local loopback port to reserve for this service later |
| `--auth-mode` | `<AUTH_MODE>` | Required auth mode for clients that use the private service Values: `bearer-env`, `header-env`, `basic-env`, `mutual-tls`, `ssh-only`. |
| `--auth-env` | `<AUTH_ENV>` | Environment variable that supplies auth material for env-backed modes |
| `--auth-header` | `<AUTH_HEADER>` | Header name for header/bearer auth modes |
| `--allow-client` | `<ALLOWED_CLIENTS>` | Allowed client label. Repeat for multiple expected clients |
| `--description` | `<DESCRIPTION>` | Human-readable description |
| `--preview-policy` | `<PREVIEW_POLICY>` | Workflow preview URL policy for this managed service Values: `none`, `always`, `on-failure`, `manual-approval`, `keep-alive-until`. |
| `--preview-keep-alive-until` | `<PREVIEW_KEEP_ALIVE_UNTIL>` | RFC3339 expiry for --preview-policy keep-alive-until |

## `homeboy tunnel service list`

```sh
homeboy tunnel service list
```

List private service tunnel declarations

## `homeboy tunnel service show`

```sh
homeboy tunnel service show <ID>
```

Show a private service tunnel declaration

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

## `homeboy tunnel service set`

```sh
homeboy tunnel service set [OPTIONS] [ID]
```

Modify a private service tunnel declaration

| Argument | Required | Description |
| --- | --- | --- |
| `[ID]` | no | Entity ID (optional if provided in JSON body) |

| Option | Value | Description |
| --- | --- | --- |
| `--json` | `<JSON>` | JSON object to merge into the entity (supports @file and - for stdin) |
| `--base64` | `<BASE64>` | Base64-encoded JSON object (bypasses shell escaping issues) |
| `--replace` | `<FIELD>` | Replace these fields instead of merging arrays |

## `homeboy tunnel service remove`

```sh
homeboy tunnel service remove <ID>
```

Remove a private service tunnel declaration

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

## `homeboy tunnel service url`

```sh
homeboy tunnel service url <ID>
```

Print the declared private local URL for a service tunnel

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

## `homeboy tunnel service status`

```sh
homeboy tunnel service status <ID>
```

Show declaration, process, health, backend, and evidence status

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

## `homeboy tunnel service start`

```sh
homeboy tunnel service start [OPTIONS] <ID>
```

Start and supervise a declared local service command

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

| Option | Value | Description |
| --- | --- | --- |
| `--command` | `<COMMAND>` | Long-running service command to execute through the platform shell |
| `--cwd` | `<CWD>` | Working directory for the service command |
| `--env` | `<ENV>` | Environment assignment passed to the service command. Repeat for multiple values |
| `--host` | `<HOST>` | Local loopback host declared for this service |
| `--port` | `<PORT>` | Local port declared for this service |
| `--scheme` | `<SCHEME>` | Local URL scheme |
| `--health-url` | `<HEALTH_URL>` | Full health-check URL to poll before reporting the service ready |
| `--health-path` | `<HEALTH_PATH>` | Health-check path appended to the declared local URL |
| `--readiness-timeout` | `<READINESS_TIMEOUT>` | Seconds to wait for the service health check |
| `--readiness-kind` | `<READINESS_KIND>` | Readiness contract label reported in service status Values: `process`, `preview`, `proof`. |
| `--require-listener` | flag | Require the declared local URL host:port to accept TCP connections |
| `--readiness-artifact` | `<READINESS_ARTIFACT>` | Artifact file whose JSON value proves readiness |
| `--readiness-artifact-json-pointer` | `<READINESS_ARTIFACT_JSON_POINTER>` | JSON Pointer inside --readiness-artifact whose value must match |
| `--readiness-artifact-json-equals` | `<READINESS_ARTIFACT_JSON_EQUALS>` | Expected string/JSON value for --readiness-artifact-json-pointer |
| `--readiness-stdout-regex` | `<READINESS_STDOUT_REGEX>` | Regex that must match captured service stdout before readiness is true |
| `--public-tunnel-backend` | `<PUBLIC_TUNNEL_BACKEND>` | Public tunnel backend adapter Values: `none`, `command`. |
| `--public-tunnel-command` | `<PUBLIC_TUNNEL_COMMAND>` | Provider-neutral backend command to supervise when using the command backend |
| `--public-tunnel-public-url` | `<PUBLIC_TUNNEL_PUBLIC_URL>` | Public URL exposed by the backend command |
| `--source-run-id` | `<SOURCE_RUN_ID>` | Owning workflow run ID to attach to preview artifacts |
| `--source-workflow-id` | `<SOURCE_WORKFLOW_ID>` | Owning workflow ID to attach to preview artifacts |

## `homeboy tunnel service stop`

```sh
homeboy tunnel service stop <ID>
```

Stop a running managed local service and cleanup runtime state

| Argument | Required | Description |
| --- | --- | --- |
| `<ID>` | yes | Service tunnel ID |

## `homeboy tunnel preview-client`

```sh
homeboy tunnel preview-client <COMMAND>
```

Connect a local preview origin to a Homeboy preview ingress

| Subcommand | Summary |
| --- | --- |
| `homeboy tunnel preview-client start` | Start an outbound authenticated reverse channel for one public host |
| `homeboy tunnel preview-client diagnose-auth` | Compare preview-client token digests without printing token material |

## `homeboy tunnel preview-client start`

```sh
homeboy tunnel preview-client start [OPTIONS]
```

Start an outbound authenticated reverse channel for one public host

| Option | Value | Description |
| --- | --- | --- |
| `--ingress` | `<INGRESS>` | Preview ingress/broker base URL |
| `--public-host` | `<PUBLIC_HOST>` | Exact public host to register. Wildcards are rejected |
| `--local-origin` | `<LOCAL_ORIGIN>` | Local HTTP(S) origin to forward requests to |
| `--session-id` | `<SESSION_ID>` | Preview session ID claimed by this client |
| `--token-env` | `<TOKEN_ENV>` | Environment variable that contains the preview tunnel bearer token |
| `--poll-timeout` | `<POLL_TIMEOUT>` | Long-poll timeout in seconds for ingress request claims |
| `--ready-stdout` | flag | Print the public preview origin to stdout after successful registration |

## `homeboy tunnel preview-client diagnose-auth`

```sh
homeboy tunnel preview-client diagnose-auth [OPTIONS]
```

Compare preview-client token digests without printing token material

| Option | Value | Description |
| --- | --- | --- |
| `--token-env` | `<TOKEN_ENV>` | Environment variable that contains the preview tunnel bearer token |
| `--token-sha256-env` | `<TOKEN_SHA256_ENV>` | Environment variable containing the allowed client token SHA-256 digest |

## `homeboy tunnel preview-ingress`

```sh
homeboy tunnel preview-ingress <COMMAND>
```

Run and inspect the VPS-side public preview ingress

| Subcommand | Summary |
| --- | --- |
| `homeboy tunnel preview-ingress install` | Render a non-destructive operator install plan for a VPS preview ingress domain |
| `homeboy tunnel preview-ingress install-status` | Render machine-readable operator install status checks without probing a live VPS |
| `homeboy tunnel preview-ingress route` | Register or replace one active public-host route |
| `homeboy tunnel preview-ingress unroute` | Remove one preview ingress route |
| `homeboy tunnel preview-ingress list` | List registered preview ingress routes |
| `homeboy tunnel preview-ingress status` | Report route lifecycle and recent server failure metadata |
| `homeboy tunnel preview-ingress serve` | Run the blocking HTTP ingress server behind a TLS terminator |

## `homeboy tunnel preview-ingress install`

```sh
homeboy tunnel preview-ingress install [OPTIONS]
```

Render a non-destructive operator install plan for a VPS preview ingress domain

| Option | Value | Description |
| --- | --- | --- |
| `--server` | `<SERVER>` | Configured Homeboy server ID for the VPS |
| `--domain` | `<DOMAIN>` | Operator-owned domain, e.g. example.com |
| `--public-host-pattern` | `<PUBLIC_HOST_PATTERN>` | Wildcard host pattern routed to the ingress, e.g. *-tunnel.example.com |
| `--bind` | `<BIND>` | Stable loopback bind address for the ingress daemon |
| `--binary-path` | `<BINARY_PATH>` | Homeboy binary path used by the service unit |
| `--service-name` | `<SERVICE_NAME>` | systemd service name |
| `--user` | `<USER>` | System user that runs the ingress service |
| `--group` | `<GROUP>` | System group that runs the ingress service |

## `homeboy tunnel preview-ingress install-status`

```sh
homeboy tunnel preview-ingress install-status [OPTIONS]
```

Render machine-readable operator install status checks without probing a live VPS

| Option | Value | Description |
| --- | --- | --- |
| `--server` | `<SERVER>` | Configured Homeboy server ID for the VPS |
| `--domain` | `<DOMAIN>` | Operator-owned domain, e.g. example.com |
| `--public-host-pattern` | `<PUBLIC_HOST_PATTERN>` | Wildcard host pattern routed to the ingress, e.g. *-tunnel.example.com |
| `--bind` | `<BIND>` | Stable loopback bind address for the ingress daemon |
| `--binary-path` | `<BINARY_PATH>` | Homeboy binary path used by the service unit |
| `--service-name` | `<SERVICE_NAME>` | systemd service name |
| `--user` | `<USER>` | System user that runs the ingress service |
| `--group` | `<GROUP>` | System group that runs the ingress service |

## `homeboy tunnel preview-ingress route`

```sh
homeboy tunnel preview-ingress route [OPTIONS] <SESSION_ID>
```

Register or replace one active public-host route

| Argument | Required | Description |
| --- | --- | --- |
| `<SESSION_ID>` | yes | Preview session ID |

| Option | Value | Description |
| --- | --- | --- |
| `--public-host` | `<PUBLIC_HOST>` | Public host routed by the TLS/proxy layer, e.g. run-123-tunnel.preview.example.test |
| `--upstream-origin` | `<UPSTREAM_ORIGIN>` | Local/reverse-channel HTTP origin for this session |
| `--expires-at` | `<EXPIRES_AT>` | RFC3339 expiry after which ingress returns 410 |
| `--inactive` | flag | Mark the route disconnected while preserving diagnostics |

## `homeboy tunnel preview-ingress unroute`

```sh
homeboy tunnel preview-ingress unroute <SESSION_ID>
```

Remove one preview ingress route

| Argument | Required | Description |
| --- | --- | --- |
| `<SESSION_ID>` | yes | Preview session ID |

## `homeboy tunnel preview-ingress list`

```sh
homeboy tunnel preview-ingress list
```

List registered preview ingress routes

## `homeboy tunnel preview-ingress status`

```sh
homeboy tunnel preview-ingress status [OPTIONS]
```

Report route lifecycle and recent server failure metadata

| Option | Value | Description |
| --- | --- | --- |
| `--bind` | `<BIND>` | Bind address to include in the status output |
| `--domain` | `<DOMAIN>` | Operator-owned preview domain |
| `--public-host-pattern` | `<PUBLIC_HOST_PATTERN>` | Public host pattern routed to this ingress |
| `--host` | `<HOST>` | Public host to inspect for preview-client registration state |

## `homeboy tunnel preview-ingress serve`

```sh
homeboy tunnel preview-ingress serve [OPTIONS]
```

Run the blocking HTTP ingress server behind a TLS terminator

| Option | Value | Description |
| --- | --- | --- |
| `--bind` | `<BIND>` | Loopback bind address for Nginx/Caddy/Cloudflare to proxy to |
| `--domain` | `<DOMAIN>` | Operator-owned preview domain |
| `--public-host-pattern` | `<PUBLIC_HOST_PATTERN>` | Public host pattern routed to this ingress |
| `--token-sha256-env` | `<TOKEN_SHA256_ENV>` | Environment variable containing the allowed client token SHA-256 digest |

## `homeboy tunnel preview-consumer`

```sh
homeboy tunnel preview-consumer <COMMAND>
```

Run a configured preview consumer with a Homeboy-owned public URL

| Subcommand | Summary |
| --- | --- |
| `homeboy tunnel preview-consumer run` | Run a command described by a preview-consumer JSON config |

## `homeboy tunnel preview-consumer run`

```sh
homeboy tunnel preview-consumer run [OPTIONS]
```

Run a command described by a preview-consumer JSON config

| Option | Value | Description |
| --- | --- | --- |
| `--config` | `<CONFIG>` | JSON config containing command, args, env, artifact, and extraction rules |
| `--service-id` | `<SERVICE_ID>` | Service ID whose started tunnel status contains the public preview URL |
| `--preview-public-url` | `<PREVIEW_PUBLIC_URL>` | Public/tunnel preview origin owned by Homeboy |
| `--artifacts-dir` | `<ARTIFACTS_DIR>` | Override the config artifact directory |
| `--non-blocking` | flag | Start the command under supervision and return as soon as the preview is ready, leaving the command running (held preview flows) |
| `--ready-timeout` | `<READY_TIMEOUT>` | Seconds to wait for the preview to report ready in non-blocking mode before returning while leaving the command running |

## `homeboy tunnel artifact-origin`

```sh
homeboy tunnel artifact-origin <COMMAND>
```

Serve the artifact root as a browser/reviewer-facing static origin

| Subcommand | Summary |
| --- | --- |
| `homeboy tunnel artifact-origin serve` | Serve Homeboy artifact-root paths with CORS headers for browser consumers |
| `homeboy tunnel artifact-origin status` | Print the artifact origin root and public URL mapping without starting a server |
| `homeboy tunnel artifact-origin inspect` | Map an artifact-origin request path or file path to its served file and public URL |
| `homeboy tunnel artifact-origin dom-boxes` | Capture DOM bounding boxes for data-figma-node-id elements in static HTML pages |

## `homeboy tunnel artifact-origin serve`

```sh
homeboy tunnel artifact-origin serve [OPTIONS]
```

Serve Homeboy artifact-root paths with CORS headers for browser consumers

| Option | Value | Description |
| --- | --- | --- |
| `--bind` | `<BIND>` | Loopback bind address for the local static artifact origin |
| `--root` | `<ROOT>` | Artifact root to serve. Defaults to Homeboy's configured artifact root |
| `--ingress` | `<INGRESS>` | Preview ingress/broker URL. With --public-host, keeps a durable outbound reverse connection open for this artifact origin |
| `--public-host` | `<PUBLIC_HOST>` | Exact public host claimed by the durable artifact origin |
| `--token-env` | `<TOKEN_ENV>` | Environment variable containing the reverse-client bearer token |
| `--poll-timeout` | `<POLL_TIMEOUT>` | Long-poll timeout in seconds for ingress request claims |

## `homeboy tunnel artifact-origin status`

```sh
homeboy tunnel artifact-origin status [OPTIONS]
```

Print the artifact origin root and public URL mapping without starting a server

| Option | Value | Description |
| --- | --- | --- |
| `--bind` | `<BIND>` | Loopback bind address expected by the local static artifact origin |
| `--root` | `<ROOT>` | Artifact root to inspect. Defaults to Homeboy's configured artifact root |

## `homeboy tunnel artifact-origin inspect`

```sh
homeboy tunnel artifact-origin inspect [OPTIONS] <PATH>
```

Map an artifact-origin request path or file path to its served file and public URL

| Argument | Required | Description |
| --- | --- | --- |
| `<PATH>` | yes | Request path, artifact-root-relative path, or filesystem path to inspect |

| Option | Value | Description |
| --- | --- | --- |
| `--root` | `<ROOT>` | Artifact root to inspect. Defaults to Homeboy's configured artifact root |
| `--fail-on-missing` | flag | Return a non-zero exit code when the mapped file is missing |

## `homeboy tunnel artifact-origin dom-boxes`

```sh
homeboy tunnel artifact-origin dom-boxes [OPTIONS]
```

Capture DOM bounding boxes for data-figma-node-id elements in static HTML pages

| Option | Value | Description |
| --- | --- | --- |
| `--root` | `<ROOT>` | Artifact directory root containing the static HTML entrypoints |
| `--entrypoint` | `<ENTRYPOINT>` | HTML entrypoint path, relative to --root; repeat for multiple pages |
| `--report` | `<REPORT>` | Write the schema payload directly to this JSON file |
| `--text-sample-limit` | `<TEXT_SAMPLE_LIMIT>` | Maximum normalized characters captured from each element text sample |

