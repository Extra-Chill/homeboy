# Structured Sidecars

Homeboy core owns the structured sidecar registry used by extension runners.

Extension manifests declare support with `structured_sidecars`. Boolean declarations use the core default path, producer, and schema version. Detailed declarations may override the path or producer for extension-specific packaging needs.

Canonical sidecar keys:

- `lint.findings` writes normalized lint finding arrays to `lint-findings.json`.
- `test.results` writes normalized test result objects to `test-results.json`.
- `test.failures` writes parsed test failure arrays to `test-failures.json`.
- `bench.results` writes benchmark result objects to `bench-results.json`.
- `trace.results` writes trace result objects to `trace.json`.
- `trace.artifacts` writes trace artifact metadata arrays under `artifacts`.
- `annotations` writes inline-review annotation arrays under `annotations`.

Core validation is intentionally shape-focused: it rejects unknown sidecar keys, wrong top-level JSON containers, non-object array items, and missing minimum required fields. Extension-specific parsers can still enforce richer producer semantics after the generic contract passes.

`bench.results` and `trace.results` additionally validate known product-neutral browser/profile/trace evidence fields against the core browser evidence schemas. See [Browser Evidence Schemas](browser-evidence-schemas.md).

Runtime helper distribution is also core-owned. Normal Homeboy extension execution injects `HOMEBOY_RUNTIME_*` helper paths, and direct invocations can resolve a helper with:

```bash
homeboy runtime helper path runner-prelude.sh
homeboy runtime helper path HOMEBOY_RUNTIME_COMMAND_CAPTURE
```
