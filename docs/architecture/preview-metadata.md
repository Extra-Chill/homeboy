# Preview Metadata

Homeboy preserves generic preview metadata and can either consume caller-supplied
public access or own a native preview tunnel lifecycle for trace workloads.
Commands, extensions, wrappers, or external integrations can emit preview facts
through environment variables, and Homeboy stores them on persisted run metadata
and renders them in report digests.

## Contract

Set `HOMEBOY_PREVIEW_JSON` to a JSON object. Recommended fields:

```json
{
  "schema": "homeboy/preview/v1",
  "provider": "dummy",
  "local_url": "http://127.0.0.1:8080",
  "public_url": "https://preview.example.test/run-1",
  "hold_seconds": 600,
  "expires_at": "2026-06-01T22:00:00Z",
  "status": "running",
  "process_id": "pid-123",
  "runtime_id": "runtime-abc",
  "cleanup_status": "pending",
  "origin_evidence": [
    {
      "schema_version": 1,
      "managed_service_id": "site-preview",
      "declared": { "host": "app.localhost", "port": 3000, "protocol": "http" },
      "local_url": "http://app.localhost:3000/",
      "public_preview_url": "https://preview.example.test/",
      "browser_requested_url": "https://preview.example.test/",
      "browser_final_url": "https://preview.example.test/?view=site",
      "window_location": {
        "origin": "https://preview.example.test",
        "hostname": "preview.example.test",
        "protocol": "https:",
        "port": "",
        "is_secure_context": true
      }
    }
  ]
}
```

`HOMEBOY_PREVIEW_PUBLIC_URL` may be set by a caller or integration that creates
public access. When present, Homeboy copies it into `public_url` if the preview
object does not already contain that field.

For the default external mode, Homeboy does not create tunnels, publish previews,
or know about product-specific runtimes. Public access is supplied by the
caller/integration.

## Trace Native Preview Tunnels

Trace workloads can request a Homeboy-native preview lifecycle instead of an
external provider command:

```json
{
  "public_preview": {
    "mode": "homeboy_native",
    "local_origin": "http://127.0.0.1:49823",
    "require_https": true,
    "native": {
      "operator_domain": "preview.example.test",
      "session_id": "wc-stripe-real-wallet",
      "ingress_url": "https://preview-broker.example.test",
      "token_env": "HOMEBOY_PREVIEW_TUNNEL_TOKEN"
    },
    "required_asset_paths": [
      "/assets/app.js"
    ]
  }
}
```

`mode: "homeboy_native"` derives `https://{session_id}-tunnel.{operator_domain}`
unless `native.public_host` or `public_origin` is supplied. Trace starts the
native client with the minimal internal command contract:

```sh
homeboy tunnel preview-client start \
  --public-host wc-stripe-real-wallet-tunnel.preview.example.test \
  --local-origin http://127.0.0.1:49823 \
  --session-id wc-stripe-real-wallet \
  --ingress https://preview-broker.example.test \
  --token-env HOMEBOY_PREVIEW_TUNNEL_TOKEN \
  --ready-stdout
```

The client must print the ready public HTTPS origin to stdout before trace
collection starts. Trace then preflights required assets through the public URL,
injects `HOMEBOY_PREVIEW_JSON`, `HOMEBOY_PREVIEW_PUBLIC_URL`, and
`HOMEBOY_TRACE_PREVIEW_*` environment values into the workload, and terminates
the client during cleanup on success or failure.

Native trace metadata uses `schema: "homeboy/preview/v1"`,
`provider: "homeboy-native"`, and includes `session_id`, `public_host`,
`ingress_url`, `client_command`, and `log_paths` when available. The concrete
ingress daemon and reverse client implementations are tracked by #4090 and
#4092; this trace contract is intentionally limited to the lifecycle seam they
must satisfy.

## Trace Public Preview Assets

Trace workloads can declare public-preview assets that must be fetchable before
trace collection starts:

```json
{
  "public_preview": {
    "local_origin": "http://127.0.0.1:49823",
    "public_origin": "https://preview.example.test",
    "require_https": true,
    "required_asset_paths": [
      "/assets/app.js",
      "/assets/app.css"
    ],
    "asset_fanout": {
      "asset_paths": [
        "/assets/app.js?ver=1",
        "/assets/app.css?ver=1",
        "/assets/vendor.js?ver=1",
        "/assets/runtime.css?ver=1"
      ],
      "concurrency": 16,
      "repeat_count": 3,
      "expected_body_contains": "fixture-asset-ok"
    }
  }
}
```

Each entry can be a public-origin-relative path or an absolute `http`/`https`
URL. Homeboy fetches the required assets through the public preview origin before
starting the trace runner. Non-2xx responses, aborted requests, and connection
errors fail fast with `public_preview.required_asset_paths` diagnostics so a run
does not collect baseline/candidate traces from a page whose static assets cannot
load.

`asset_fanout` is the native preview tunnel stress gate for browser/static asset
bursts. Homeboy expands each listed path against the public preview origin,
requests every path `repeat_count` times with up to `concurrency` concurrent
workers, and fails before trace collection when any request aborts, times out,
returns a non-2xx status such as `502`/`504`, or misses the optional
`expected_body_contains` text. The preview metadata records a
`homeboy/preview-asset-fanout/v1` report with client request totals, status
counts, failure buckets, and per-request rows. Native ingress/client/local-origin
implementations can populate the optional ingress and local-origin request count
fields in the same report so reviewer artifacts can distinguish browser/client
errors from tunnel ingress or origin delivery gaps.

Use this core gate for generic HTTP asset fanout proof. Product-specific proofs,
such as Sample Runtime or WooCommerce asset lists for Extra-Chill/homeboy#4062,
should live in the owning rig or extension and consume this generic
`asset_fanout` contract.

## Persistence

When a command records an observation run, Homeboy stores the object at
`metadata.preview`. Lab offload forwards `HOMEBOY_PREVIEW_JSON` and
`HOMEBOY_PREVIEW_PUBLIC_URL` to the runner so offloaded commands preserve the
same caller-supplied preview facts.

## Reporting

`homeboy report performance-digest` renders scalar preview fields in a
`Preview` section, including local URL, public URL, hold/expiry, lifecycle
status, runtime/process ID, and cleanup status when available.

Bench comparison side-by-side reports also consume structured preview artifacts
when scenario or run artifacts use `type: "preview"`, `kind: "preview"`, the
artifact name `preview`, or preview-specific fields such as `preview_url`,
`public_url`, `local_url`, `status`, `expires_at`, `cleanup_status`,
`service_lifecycle`, and `browser_origin_evidence`. The rendered
`reports.side_by_side.rigs[].preview_links[]` table labels links by explicit
artifact `role` when present; otherwise comparison order infers `baseline` for
the first rig, `candidate` for the second rig, and `provider` for additional
rigs.

When `origin_evidence` or `browser_origin_evidence` is present in preview
metadata, `homeboy report performance-digest` also renders the effective browser
origin details. This gives managed-service proof artifacts enough context to
diagnose hostname-sensitive routing, redirects, and secure-context behavior.
