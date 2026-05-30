# Homeboy Finding Contract

`HomeboyFinding` is the shared typed finding model for Homeboy commands,
extensions, reports, and agents. It is analogous to `HomeboyPlan`: producers
build one normalized shape first, then storage and output layers project it into
their local representation.

## Core Shape

Common fields stay stable across producers:

- `tool`: producer tool family such as `lint`, `audit`, `test`, or `budget`.
- `rule`: machine-readable rule, sniff, audit kind, test cluster, or budget code.
- `category`: broad grouping such as `security`, `code_audit`, `test_failure`, or `budget`.
- `severity`: producer severity as a string so extensions can preserve their own taxonomy.
- `fingerprint`: stable identity used for baselines, dedupe, and trend tracking.
- `location.file`, `location.line`, `location.column`: optional source location.
- `message`: human-facing summary.
- `fixable`: whether automated or guided remediation is available.
- `producer`: command, extension, step, sidecar, and artifact provenance.
- `metadata`: structured source-specific common details.
- `raw`: lossless source payload when the producer has richer native data.

## Storage Projection

`NewFindingRecord` and `FindingRecord` are observation-store records. They are
not the producer contract. Use `NewFindingRecord::from_homeboy_finding()` when a
finding needs to be persisted.

The projection stores high-value query fields as SQLite columns:

- `tool`, `rule`, `file`, `line`, `severity`, `fingerprint`, `message`, `fixable`

The projection stores remaining normalized evidence in `metadata_json`:

- `category`
- `producer`
- `source_sidecar`
- `source_artifact`
- source-specific `metadata`
- `raw`

This keeps existing run queries stable while making the upstream contract clear.

## Representative Producers

The contract can represent current finding dialects without forcing one PR to
migrate every producer:

- lint findings map sniffs/rules, category, source file, severity, fixability,
  and `lint-findings` sidecar provenance.
- audit findings map audit kind, convention, confidence, file, suggestion, and
  `audit-findings` sidecar provenance.
- test failures map failed test name, source location, failure category, cluster,
  and error type into `metadata`.
- bench budget findings map budget code, actual/expected/unit, subject,
  severity, and gate status.
- annotation sidecars map source, code, location, severity, fixability, and raw
  annotation payloads.

Follow-up PRs should migrate producers incrementally to emit `HomeboyFinding`
directly at their command/output boundary.
