# `homeboy tunnel`

## Synopsis

```sh
homeboy tunnel service <COMMAND>
homeboy tunnel preview-client <COMMAND>
homeboy tunnel preview-ingress <COMMAND>
homeboy tunnel preview-consumer <COMMAND>
```

`tunnel` manages Homeboy-native private service tunnel declarations, local managed service lifecycle, and the VPS-side public preview ingress used by generic preview URLs. Homeboy can start a long-running local command, record safe command/process/log evidence, report readiness, and stop the process group without relying on an external chat or tunnel wrapper.

`preview-client` connects a local/lab preview origin to a Homeboy-owned preview ingress over an outbound authenticated reverse channel. It is the local side of native public browser preview tunnels and does not use external tunnel providers.

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

The declaration requires an explicit auth mode and stores a private-loopback policy. It does not expose a public unauthenticated URL. `--preview-policy` defaults to `none`; workloads can opt into `always`, `on-failure`, `manual-approval`, or `keep-alive-until` when public preview artifacts are useful.

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

Expose the managed service through a provider-neutral backend command:

```sh
homeboy tunnel service start context-a8c \
  --command 'npm run dev -- --host 127.0.0.1 --port 7331' \
  --cwd /path/to/workspace \
  --host 127.0.0.1 \
  --port 7331 \
  --health-path / \
  --public-tunnel-backend command \
  --public-tunnel-command './tools/open-preview-tunnel.sh' \
  --public-tunnel-public-url 'https://preview.example.test/run-123'
```

The `command` backend is a generic adapter seam. Homeboy starts and supervises the backend command, injects `HOMEBOY_SERVICE_ID`, `HOMEBOY_SERVICE_LOCAL_URL`, and `HOMEBOY_TUNNEL_PUBLIC_URL`, records backend PID/process/log evidence, and stops it with the managed service. Provider-specific behavior such as Traforo, Cloudflare, ngrok, or a Homeboy VPS broker belongs in the backend command or a future extension, not in Homeboy core semantics.

When a service's preview policy is relevant, `service status` and `service start` include a structured `preview` artifact with schema `homeboy/preview-url/v1`. The artifact records the service ID, local URL, optional public URL, backend, policy, cleanup/expiry metadata, and owning run/workflow IDs when the start command supplied them.

## Preview Consumer Orchestration

`preview-consumer run` executes a configured command that needs a Homeboy-owned public preview URL. The consumer contract is configuration-based: Homeboy supplies `${preview_public_url}` and `${artifacts_dir}` template values, runs the configured command, and extracts a reviewer-clickable result URL using the configured stdout prefix or JSON pointer.

Example `preview-consumer.json`:

```json
{
  "id": "sample-preview-consumer",
  "command": {
    "program": "node",
    "args": [
      "tools/open-consumer.mjs",
      "--public-url",
      "${preview_public_url}",
      "--artifacts",
      "${artifacts_dir}"
    ],
    "cwd": "/workspace/project",
    "artifacts_dir": "artifacts/sample-preview-consumer"
  },
  "output": {
    "public_result_json_file": "summary.json",
    "public_result_json_pointer": "/public_result_url",
    "public_result_stdout_prefix": "Public result URL:"
  }
}
```

Use `--service-id` when `homeboy tunnel service start` has already recorded a `public_url` for the held preview service:

```sh
homeboy tunnel preview-consumer run \
  --config ./preview-consumer.json \
  --service-id site-preview
```

Use `--preview-public-url` when the orchestrating workload already has the public origin in hand:

```sh
homeboy tunnel preview-consumer run \
  --config ./preview-consumer.json \
  --preview-public-url https://run-123-tunnel.example.net
```

The output artifact uses schema `homeboy/preview-consumer-run/v1` and records the consumer ID, public preview URL, optional service ID, artifact directory, optional `public_result_url`, stdout, stderr, exit code, and artifact path. Preview interpretation stays in the configured consumer; Homeboy only owns public URL lifecycle, command execution, and durable evidence capture.

## Preview Client

Start a native outbound reverse preview client for one exact public host:

```sh
homeboy tunnel preview-client start \
  --ingress https://preview-broker.example \
  --public-host run42-tunnel.example.test \
  --local-origin http://127.0.0.1:49822 \
  --token-env HOMEBOY_PREVIEW_TUNNEL_TOKEN
```

The client registers exactly one public host; wildcard public hosts are rejected so a lab runtime cannot implicitly claim a whole domain. `--local-origin` must be an HTTP(S) origin supplied by the caller, commonly a local development server or lab-managed preview service.

The preview ingress contract is JSON-over-HTTP with bearer auth from `--token-env`:

- `POST /preview/client/register`: register `{ public_host, local_origin }`.
- `POST /preview/client/next`: long-poll for one request with `{ public_host, timeout_secs }`.
- `POST /preview/client/respond`: return `{ public_host, response }` for a request.
- `POST /preview/client/close`: mark the public host session closed on shutdown.

Ingress requests carry `request_id`, `method`, `path`, selected `headers`, and optional `body_base64`. Client responses carry `request_id`, `status`, response `headers`, `body_base64`, and optional structured `error`. Local-origin failures are returned as `502` responses with `error.kind` such as `local_origin_request_failed`, distinct from ingress/channel failures logged by the client process.

Each claimed ingress request is forwarded on a worker thread so browser static asset fanout can be served concurrently. Hop-by-hop headers are filtered before forwarding to the local origin and before returning the local response to ingress.

## Preview Ingress

`preview-ingress` is the VPS-side HTTP daemon surface for Homeboy-native public preview tunnels. It is designed to run behind an operator-managed TLS/proxy layer such as Nginx, Caddy, or Cloudflare:

```text
Client
  -> https://{id}-tunnel.<operator-domain>
  -> TLS/proxy layer
  -> homeboy tunnel preview-ingress serve --bind 127.0.0.1:7350
  -> active Homeboy preview route
  -> local/reverse-channel HTTP origin
```

The core contract is generic HTTP ingress: public host/session routing, reverse-channel-compatible HTTP origins, request/response streaming, status, logs, and cleanup. Workload-specific behavior, asset health policy, and preview interpretation belongs in Homeboy Extensions or `homeboy-rigs`, not in Homeboy core.

Render a non-destructive operator install plan for a wildcard preview ingress domain:

```sh
homeboy tunnel preview-ingress install \
  --server chubes-net \
  --domain chubes.net \
  --public-host-pattern '*-tunnel.chubes.net' \
  --service-name homeboy-preview-ingress
```

The install command is plan-first. It does not SSH to the server, write files, reload proxies, or deploy anything. It emits machine-readable JSON containing:

- a systemd unit for the preview ingress service
- Nginx and Caddy reverse proxy snippets for the supplied wildcard host pattern
- DNS, loopback status, and public status smoke-check commands
- systemd status, restart, and rollback commands
- the exact operator configuration still required before the plan can be applied
- a non-secret policy note for token/pairing material

Render the install status contract without probing the live VPS:

```sh
homeboy tunnel preview-ingress install-status \
  --server chubes-net \
  --domain chubes.net \
  --public-host-pattern '*-tunnel.chubes.net'
```

Install status output records planned checks so operators and future apply/probe flows can share one output shape. It includes `systemctl is-active`, `systemctl status`, loopback ingress status, wildcard DNS, and public ingress status commands.

The required VPS/operator inputs are intentionally explicit and non-secret:

- a configured Homeboy server ID with SSH access to the VPS
- wildcard DNS for the host pattern pointing at the VPS public address
- TLS certificate coverage for the wildcard preview host pattern
- a Homeboy binary path on the VPS
- a system user/group for the service
- one reverse proxy choice, Nginx or Caddy

Secrets are not rendered into the generated unit or proxy snippets. Pairing tokens/client credentials belong in Homeboy secret/config surfaces before enabling live routes.

Register one active preview route:

```sh
homeboy tunnel preview-ingress route run-123 \
  --public-host run-123.preview.example.net \
  --upstream-origin http://127.0.0.1:7331 \
  --expires-at 2026-06-12T03:30:00Z
```

Use `--inactive` to retain a route record for diagnostics while making the ingress return `410 disconnected_session`.

Run the ingress daemon:

```sh
homeboy tunnel preview-ingress serve \
  --domain preview.example.net \
  --bind 127.0.0.1:7350 \
  --public-host-pattern '*.preview.example.net'
```

The daemon routes by `Host`, handles concurrent preview requests in separate worker threads, proxies request bodies to the configured upstream origin, streams upstream response bodies back to the client, and preserves response status plus non-hop-by-hop headers such as `content-type` and cache headers.

Diagnostics are structured so generic preview workloads can distinguish ingress and upstream problems:

- `404 missing_session`: no active route matched the requested host.
- `410 expired_session`: the route's RFC3339 expiry has passed.
- `410 disconnected_session`: the route is retained but marked inactive.
- `502 upstream_error`: the upstream origin failed before response streaming.
- `504 upstream_timeout`: the upstream origin timed out.

Each request writes a JSON line to stderr with request ID, host, path, status, bytes, duration, and classification. `/_homeboy/preview-ingress/status` returns the current route status as JSON from the running daemon.

This is the ingress side of #4089 and the first Homeboy-owned replacement path for #4062's current tunnel-provider blocker. Auth/pairing/token lifecycle and the authenticated reverse preview client remain generic surfaces; the ingress route's `upstream_origin` is the HTTP seam those clients attach to.

## Native Preview Tunnel Auth Model

Homeboy-native preview ingress uses a separate auth contract from reverse runner jobs. Runner broker auth proves which lab can claim and finish runner work; preview tunnel auth proves which client may claim a public preview host/session and forward requests over a reverse channel to a loopback origin.

The native preview auth policy lives under `policy.native_preview_auth` on a service tunnel declaration. It stores only token metadata and SHA-256 token digests, never plaintext token material:

```json
{
  "policy": {
    "native_preview_auth": {
      "require_client_token": true,
      "default_session_ttl_secs": 900,
      "max_session_ttl_secs": 3600,
      "allowed_public_hosts": [ "*.preview.example.net" ],
      "allowed_session_ids": [ "run-123" ],
      "tokens": [
        {
          "id": "lab-client-1",
          "token_sha256": "<sha256 digest>",
          "allowed_clients": [ "local-lab" ],
          "allowed_public_hosts": [ "run-123.preview.example.net" ],
          "allowed_session_ids": [ "run-123" ],
          "revoked": false,
          "expires_at": "2026-06-07T13:00:00Z"
        }
      ]
    }
  }
}
```

An ingress/client pairing request is valid only when all of these claims match:

- `client_id` is allowed by the matched token.
- `token` hashes to a configured, unrevoked, unexpired token digest.
- `public_host` matches both the service policy and token host scopes. Exact hostnames and glob patterns are supported.
- `session_id` matches both the service policy and token session scopes when scopes are configured.
- `local_origin` is an `http://` loopback origin such as `http://127.0.0.1:7331`.
- The granted lease expires at the requested TTL capped by `max_session_ttl_secs`.

Auth failures return structured validation errors for the failing claim (`token`, `client_id`, `public_host`, `session_id`, or `local_origin`). Token values are request inputs only and are not serialized in declarations, status, preview artifacts, logs, or diagnostics.

Safe operator setup for a VPS wildcard domain:

1. Configure wildcard DNS and TLS for the preview ingress host, for example `*.preview.example.net`.
2. Declare the preview service with `homeboy tunnel service expose` and keep `policy.require_auth=true`.
3. Generate high-entropy client token material outside normal logs, store the token itself in Homeboy's keychain/secret/env surface for the preview client, and store only the SHA-256 digest in `policy.native_preview_auth.tokens`.
4. Scope each token to the smallest useful client, host pattern, and session ID.
5. Use short `expires_at` and session TTLs for short-lived preview sessions; revoke by removing the token entry or setting `revoked=true`.

The current layer validates config, token, host, session, origin, and lease semantics. The actual VPS ingress route table and reverse-channel forwarding endpoints are the integration points for the native ingress/client work tracked separately from this command contract.

## Subcommands

- `service expose`: create or replace a private service tunnel declaration.
- `service list`: list declarations.
- `service show <id>`: show one declaration.
- `service set <id> ...`: update fields using the standard dynamic set contract.
- `service remove <id>`: delete a declaration.
- `service url <id>`: print the declared loopback URL.
- `service start <id>`: start and supervise a declared local service command and optional provider-neutral public tunnel backend.
- `service status <id>`: report declaration, process, local URL, public URL when present, health, backend, and log evidence state.
- `service stop <id>`: terminate the managed process group and remove runtime state while leaving log evidence files in place.
- `preview-client start`: connect a local HTTP(S) preview origin to a Homeboy preview ingress for one public host.
- `preview-ingress install`: render a non-destructive VPS preview ingress install plan.
- `preview-ingress install-status`: render machine-readable VPS preview ingress install status checks.
- `preview-ingress route <session-id>`: register or replace a host-routed preview session.
- `preview-ingress unroute <session-id>`: remove a preview route.
- `preview-ingress list`: list route records.
- `preview-ingress status`: report route lifecycle metadata.
- `preview-ingress serve`: run the blocking VPS-side HTTP ingress daemon.
- `preview-consumer run`: execute a configured consumer command with a Homeboy-owned public preview URL and record durable evidence.
