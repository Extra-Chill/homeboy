# Managed Preview Extension

`managed-preview` contains provider command helpers for Homeboy managed service previews.

Homeboy core owns the generic lifecycle contract: start a local service, supervise an optional backend command, record logs, and expose a `homeboy/preview-url/v1` artifact. This extension owns provider command glue that can be used with Homeboy's generic `--public-tunnel-backend command` seam.

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

## Hostname-Sensitive Proofs

Some consumers need the browser-effective origin to remain the local development origin. The WPCOM Calypso `/start` proof is one of these: it expects `http://calypso.localhost:3000`, not a tunnel origin.

For those consumers, pass `--expected-effective-origin` and `--require-host-preservation`. The helper fails before registering a backend that cannot prove it preserves the browser origin, returning structured blocker JSON on stderr. This keeps reviewer-clickable preview support from being mistaken for exact hostname-preserving proof.

```sh
node scripts/public-preview-backend.mjs \
  --local-url http://calypso.localhost:3000 \
  --public-url https://preview-broker.example/runs/run-123 \
  --broker-url https://preview-broker.example/api/managed-previews \
  --expected-effective-origin http://calypso.localhost:3000 \
  --expected-config-hostname calypso.localhost \
  --require-host-preservation \
  --dry-run
```

Until a broker endpoint exists and returns hostname-preserving evidence for `calypso.localhost:3000`, the Calypso `/start` proof should remain blocked rather than downgraded to a different-host tunnel proof.
