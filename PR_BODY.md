## Summary
#16617 traced recurring SQLite writer contention to oversized run metadata, not an insufficient retry budget: multi-megabyte gate-feedback diffs were copied into the same promotion report several times and rewritten together. Run metadata now externalizes the known large Cook payloads and keeps content-addressed references, while replay, review and gate-feedback consumers resolve content on demand.

## What changed
- Values larger than 16 KiB at `gate_feedback_baseline.current_diff` and the promotion-record locations move to `metadata-payloads/<sha256>`; small values remain inline without payload writes or fsync. References contain only SHA-256 and byte length, resolved under the artifact root.
- `promotion`, `promotions[]` and promotion operation results share canonical content-addressed reports. Run writes enforce a 256 KiB metadata cap with a typed error identifying the largest remaining JSON path; metadata parsing and ordinary inspection do not hydrate payloads.
- Artifact cleanup marks references from surviving run rows and sweeps unreferenced payload files, including those orphaned by run deletion. Publication/write guards prevent cleanup from sweeping payloads between publication and row commit.
- `runs reconcile --compact-metadata` explicitly externalizes existing metadata and then VACUUMs the store; routine run deletion and artifact cleanup never compact or VACUUM. Slow transaction diagnostics use the existing logging surface with structured operation, duration, changed-row and SQLite page-write byte fields.
- The >1 MB promotion replay regression checks bounded stored metadata, exact gate-feedback content and review resolution. A cleanup regression checks that deleted-run payloads are swept while referenced payloads survive; the obsolete inline checkpoint-baseline reconstruction reader and its test are removed.

## Compatibility
Additive inline/reference storage contract for small/large known payloads. Oversized unknown run metadata is now rejected instead of silently growing SQLite rows; use explicit compaction on an idle store for existing rows.

## Used for
AI read the issue, implemented payload storage and lifecycle changes, updated consumers, and exercised the regression gates.

Closes #16617
