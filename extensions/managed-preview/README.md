# Managed Preview Extension

`managed-preview` contains provider command helpers for Homeboy managed service previews.

Homeboy core owns the generic lifecycle contract: start a local service, supervise an optional backend command, record logs, and expose a `homeboy/preview-url/v1` artifact. This extension owns provider command glue that can be used with Homeboy's generic `--public-tunnel-backend command` seam.

## Browser Trace Public Preview Preflight

Browser trace workloads that require a public HTTPS origin can use
`scripts/public-preview-preflight.mjs` as the shared contract between the runtime,
the tunnel operator, and the trace runner. The helper accepts or allocates a
selected preview port, normalizes the local preview origin and public URL,
preflights both routes, and writes redacted `homeboy/public-preview-preflight/v1`
metadata for trace artifacts.

Prepare a runtime port before the service starts:

```sh
node scripts/public-preview-preflight.mjs \
  --allocate-port \
  --prepare-only \
  --metadata-file "$HOMEBOY_TRACE_ARTIFACT_DIR/public-preview-prepared.json"
```

The prepared metadata has `status: "prepared"`; pass its
`local_preview.port` back into the runtime and tunnel command. Before browser
launch, rerun the helper with the selected port and public URL so it can fail
fast on routing mismatches.

```sh
node scripts/public-preview-preflight.mjs \
  --preview-port 49822 \
  --local-url http://127.0.0.1:49822 \
  --public-url https://public-preview.example/runs/run-123 \
  --tunnel-provider traforo \
  --tunnel-session-id run-123 \
  --preflight-path / \
  --metadata-file "$HOMEBOY_TRACE_ARTIFACT_DIR/public-preview.json"
```

The helper fails before browser launch when:

- the public URL is not HTTPS;
- the selected preview port does not match the local runtime URL;
- the local runtime is not reachable on the selected port;
- the public URL does not return a successful preflight response; or
- an expected status/text check does not match both local and public routes.

The metadata intentionally records only redacted URLs, origins, port/session
identifiers, response status, content type, body byte counts, and body hashes.
Probe bodies, credentials, query strings, and fragments are not written.

This helper does **not** own tunnel process lifecycle. Homeboy core, an operator
wrapper, or a future tunnel provider is responsible for starting and keeping the
SSH forward / Traforo / broker session alive. `managed-preview` owns the
upstream-facing diagnostics contract so browser trace rigs can opt in without
copying custom environment-variable glue.

Relay-only preview ingress should be treated as disposable routing
infrastructure. It must not own run state, artifact contents, viewer metadata, or
cleanup semantics for published evidence. Operators should clean up relay
processes, forwarding sessions, DNS/route registrations, and temporary access
rules after the Homeboy-managed service stops, while keeping durable artifacts
available from the artifact store until that store's retention policy expires.

## Hostname-Preserving Broker Backend

Use the helper as Homeboy's backend command when a preview broker can create a reviewer-accessible session while preserving the browser-effective origin inside that session:

```sh
homeboy tunnel service start site-preview \
  --command 'npm run dev -- --host 127.0.0.1 --port 7331' \
  --host 127.0.0.1 \
  --port 7331 \
  --health-path / \
  --public-tunnel-backend command \
  --public-tunnel-public-url 'https://preview-broker.example/runs/run-123' \
  --public-tunnel-command 'node ~/.config/homeboy/extensions/managed-preview/scripts/public-preview-backend.mjs --expected-effective-origin http://127.0.0.1:7331 --require-host-preservation'
```

Homeboy injects `HOMEBOY_SERVICE_LOCAL_URL` and `HOMEBOY_TUNNEL_PUBLIC_URL` into the backend process. Set `HOMEBOY_PREVIEW_BROKER_URL` to the external broker endpoint. The helper registers the preview target with the broker and stays alive until Homeboy stops the managed service.

The broker response must prove `capabilities.hostname_preserving_browser_origin === true` and return browser-origin evidence matching the requested effective origin. A plain port tunnel that changes `window.location.origin` is rejected.

Use `--target-id`, `--target-url`, and repeated `--route name=url` values to carry workload-owned proof targets into broker evidence without teaching this extension product semantics. For WPCOM, the rig can keep the exact `wordpress.com/ai` landing-page proof separate from Calypso `/start` and `/setup/ai-site-builder` proof:

```sh
node scripts/public-preview-backend.mjs \
  --target-id wpcom-ai-landing \
  --target-url https://wordpress.com/ai/ \
  --route landing=https://wordpress.com/ai/ \
  --route builder_handoff=https://wordpress.com/setup/ai-site-builder \
  --local-url http://127.0.0.1:7331 \
  --public-url https://preview-broker.example/runs/run-123 \
  --broker-url https://preview-broker.example/api/managed-previews \
  --dry-run
```

## Hostname-Sensitive Proofs

Some consumers need the browser-effective origin to remain the local development origin. The WPCOM Calypso `/start` proof is one of these: it expects `http://calypso.localhost:3000`, not a tunnel origin.

For those consumers, pass `--expected-effective-origin` and `--require-host-preservation`. The helper fails before registering a backend that cannot prove it preserves the browser origin, returning structured blocker JSON on stderr. This keeps reviewer-clickable preview support from being mistaken for exact hostname-preserving proof.

```sh
node scripts/public-preview-backend.mjs \
  --target-id calypso-start \
  --target-url http://calypso.localhost:3000/start \
  --route start=http://calypso.localhost:3000/start \
  --route builder_handoff=http://calypso.localhost:3000/setup/ai-site-builder \
  --local-url http://calypso.localhost:3000 \
  --public-url https://preview-broker.example/runs/run-123 \
  --broker-url https://preview-broker.example/api/managed-previews \
  --expected-effective-origin http://calypso.localhost:3000 \
  --expected-config-hostname calypso.localhost \
  --require-host-preservation \
  --dry-run
```

Until a broker endpoint exists and returns hostname-preserving evidence for `calypso.localhost:3000`, the Calypso `/start` proof should remain blocked rather than downgraded to a different-host tunnel proof.
