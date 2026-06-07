# Managed Preview Extension

`managed-preview` contains provider command helpers for Homeboy managed service previews.

Homeboy core owns the generic lifecycle contract: start a local service, supervise an optional backend command, record logs, and expose a `homeboy/preview-url/v1` artifact. This extension owns provider command glue that can be used with Homeboy's generic `--public-tunnel-backend command` seam.

## Kimaki Backend

Use the helper as Homeboy's backend command when the public URL is known up front:

```sh
homeboy tunnel service start site-preview \
  --command 'npm run dev -- --host 127.0.0.1 --port 7331' \
  --host 127.0.0.1 \
  --port 7331 \
  --health-path / \
  --public-tunnel-backend command \
  --public-tunnel-public-url 'https://my-preview.kimaki.dev' \
  --public-tunnel-command 'node ~/.config/homeboy/extensions/managed-preview/scripts/public-preview-backend.mjs --provider kimaki --tunnel-id my-preview'
```

Homeboy injects `HOMEBOY_SERVICE_LOCAL_URL` and `HOMEBOY_TUNNEL_PUBLIC_URL` into the backend process. The helper maps those to `kimaki tunnel --port <port> --host <host> --tunnel-id <id>` and stays alive until Homeboy stops the managed service.

## Hostname-Sensitive Proofs

Some consumers need the browser-effective origin to remain the local development origin. The WPCOM Calypso `/start` proof is one of these: it expects `http://calypso.localhost:3000`, not a tunnel origin.

For those consumers, pass `--expected-effective-origin` and `--require-host-preservation`. The helper fails before starting a provider that would change the browser origin, returning structured blocker JSON on stderr. This keeps reviewer-clickable preview support from being mistaken for exact hostname-preserving proof.

```sh
node scripts/public-preview-backend.mjs \
  --provider kimaki \
  --local-url http://calypso.localhost:3000 \
  --public-url https://example.kimaki.dev \
  --expected-effective-origin http://calypso.localhost:3000 \
  --expected-config-hostname calypso.localhost \
  --require-host-preservation \
  --dry-run
```

Until a provider can serve a reviewer-clickable public URL while preserving that browser-effective origin, the Calypso `/start` proof should remain blocked rather than downgraded to a different-host tunnel proof.
