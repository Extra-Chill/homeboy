# Browser Result Shapes

Homeboy extensions share a small product-neutral vocabulary for browser,
profile, and trace results in `scripts/lib/browser-result-shapes.cjs` and
`scripts/lib/browser-result-shapes.mjs`.

These helpers describe transportable shapes only. Product semantics stay in the
extension that owns them. For example, WordPress REST route normalization,
preload attribution, and route grouping remain in `wordpress`.

## Shared Helpers

- `normalizeBrowserPerformanceProfile(profile)` returns a stable profile
  envelope with `schema_version`, `page_url`, `summary`, timing arrays,
  `phase_marks`, and computed `phases`.
- `normalizeBrowserProfileTimings(profile, { normalizeUrl })` adapts a profile's
  `resources` plus `network` rows into timing rows suitable for correlation.
- `normalizeBrowserTiming(entry, { normalizeUrl })` adapts loose resource or
  network rows to `{ url, normalizedUrl, method, status, startTime, ttfbMs,
  durationMs, initiatorType, phase, raw }`.
- `normalizeBrowserArtifact(artifact)` returns stable artifact metadata with
  `path`, optional `kind`, and optional `label`.
- `normalizeBrowserBottleneck(row)` returns stable bottleneck rows with `kind`,
  `phase`, `message`, and optional `data`.
- `normalizeTraceEvent()`, `normalizeTraceAssertion()`, and
  `normalizeTraceEnvelope()` describe generic trace timelines and assertions.

## Shape Boundary

Shared browser result shapes are intentionally narrow:

- **Network request rows** capture URL, method, resource type, status, failure,
  start time, and duration.
- **Resource timing rows** preserve browser timing fields and become correlation
  rows through `normalizeBrowserTiming()`.
- **Timeline phases** are phase marks plus computed phase windows.
- **Artifacts** describe where evidence lives and what broad kind it is.
- **Bottlenecks** describe generic report rows without product-specific meaning.

Callers pass their own `normalizeUrl` when URL semantics matter. WordPress uses
that hook to preserve its existing REST/cache-busting behavior without moving
route semantics into shared code.

## Current Consumers

- `nodejs/scripts/bench/browser-helper.mjs` emits browser profile, artifact, and
  bottleneck shapes through the shared helpers.
- `nodejs/scripts/trace/lib/timeline.mjs` emits trace events, assertions,
  artifacts, and envelopes through the shared helpers.
- `wordpress/lib/timing-correlator.js` adapts browser profiles through the shared
  timing helpers while keeping WordPress-specific URL normalization local.
