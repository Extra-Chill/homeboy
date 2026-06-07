# Browser Evidence Schemas

Homeboy core owns the product-neutral browser, profile, and trace evidence
schema vocabulary. Extensions can normalize product-specific data into these
shapes, but route semantics, application labels, and attribution rules stay in
the extension that owns that product surface.

## Version

Current schema version: `1`.

## Core Shapes

- `BrowserPerformanceProfileEnvelope` describes one captured browser profile:
  `schema_version`, `page_url`, generic summary data, timing arrays, network
  rows, console/page error arrays, paint/layout/task arrays, `phase_marks`, and
  computed `phases`.
- `BrowserNetworkRequestRow` describes one network request with URL, method,
  resource type, HTTP status, failure flag, start time, duration, and optional
  failure text.
- `BrowserTimingRow` describes a correlation row derived from browser resources
  or network requests. URL normalization is caller-owned so product semantics do
  not move into core.
- `BrowserPhaseMark` and `BrowserPhaseWindow` describe timeline phases without
  assigning product meaning to phase names.
- `BrowserArtifactMetadata` describes evidence location and broad kind with
  `path`, optional `kind`, and optional `label`.
- `BrowserOriginEvidence` describes the effective browser origin exercised for a
  managed preview service: owning `managed_service_id`, optional
  `preview_artifact_id` and `run_id`, declared host/port/protocol, local URL,
  public preview URL, browser requested URL, browser final URL, redirects,
  `window.location` origin/hostname/protocol/port, and `window.isSecureContext`.
- `BrowserBottleneckRow` describes generic report rows with `kind`, `phase`,
  `message`, and optional `data`.
- `TraceEvent`, `TraceAssertion`, and `TraceEnvelope` describe generic trace
  timelines, assertions, artifacts, status, summary, and optional failure data.

## Sidecar Validation

Structured sidecar validation stays shape-focused first. For `bench.results`
and `trace.results`, core also validates known browser evidence fields when
they are present:

- `bench.results`: `browser_profiles`, `profiles`, `network`, `artifacts`,
  `bottlenecks`, `timings`, `origin_evidence`, and
  `browser_origin_evidence`.
- `trace.results`: `timeline`, `assertions`, `artifacts`, `traces`,
  `origin_evidence`, and `browser_origin_evidence`.

`origin_evidence` is the preferred field. `browser_origin_evidence` is accepted
as an explicit alias for producers that need a more self-describing top-level
key.

## Managed Preview Origin Evidence

Preview producers should emit one `BrowserOriginEvidence` row per browser probe
that exercises a managed service. The row should capture both declared routing
facts and browser-observed facts so hostname-sensitive failures can distinguish
`app.localhost:3000`, `localhost:3000`, `127.0.0.1:3000`, and public tunnel
origins.

Minimal example:

```json
{
  "origin_evidence": [
    {
      "schema_version": 1,
      "managed_service_id": "site-preview",
      "preview_artifact_id": "preview-artifact-1",
      "run_id": "trace-run-1",
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
      },
      "redirects": [
        {
          "from_url": "https://preview.example.test/",
          "to_url": "https://preview.example.test/?view=site",
          "status": 302
        }
      ],
      "network_origin": { "tunnel": "homeboy-managed" }
    }
  ]
}
```

The same array can also be nested under `metadata.preview.origin_evidence` when
the capture belongs to a preview lifecycle record instead of a bench or trace
result sidecar. `homeboy report performance-digest` renders those rows in a
`Browser Origin Evidence` section.

Unknown fields are allowed at the sidecar envelope level so existing benchmark
and trace producers can carry additional domain-specific payloads. Known fields
must match the core product-neutral schemas.

## Boundary

Core owns transportable evidence contracts. Extensions own normalization and
meaning. For example, WordPress REST route grouping, Site Editor attribution,
Studio workflow labels, and WooCommerce scenario semantics remain outside core.
