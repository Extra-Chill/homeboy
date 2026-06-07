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
- `BrowserBottleneckRow` describes generic report rows with `kind`, `phase`,
  `message`, and optional `data`.
- `TraceEvent`, `TraceAssertion`, and `TraceEnvelope` describe generic trace
  timelines, assertions, artifacts, status, summary, and optional failure data.

## Sidecar Validation

Structured sidecar validation stays shape-focused first. For `bench.results`
and `trace.results`, core also validates known browser evidence fields when
they are present:

- `bench.results`: `browser_profiles`, `profiles`, `network`, `artifacts`,
  `bottlenecks`, and `timings`.
- `trace.results`: `timeline`, `assertions`, `artifacts`, and `traces`.

Unknown fields are allowed at the sidecar envelope level so existing benchmark
and trace producers can carry additional domain-specific payloads. Known fields
must match the core product-neutral schemas.

## Boundary

Core owns transportable evidence contracts. Extensions own normalization and
meaning. For example, WordPress REST route grouping, Site Editor attribution,
Studio workflow labels, and WooCommerce scenario semantics remain outside core.
