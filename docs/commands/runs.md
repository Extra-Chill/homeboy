# Runs Command

Inspect and maintain persisted observation-store runs and artifacts.

## Synopsis

```bash
homeboy runs list [--runner <runner-id>] [--kind bench|rig|trace] [--component <id>] [--rig <id>] [--status <status>] [--limit 20] [--include-active-runner-jobs]
homeboy runs distribution --field <metadata.path> [--kind bench] [--component <id>] [--rig <id>] [--scenario <id>] [--status <status>] [--limit 20]
homeboy runs compare [--kind bench] [--component <id>] [--rig <id>] [--scenario <id>] [--metric <name>] [--limit 20] [--format table|json]
homeboy runs show <run-id> [--json]
homeboy runs resume-plan <run-id>
homeboy runs artifacts <run-id>
homeboy runs refs [--kind bench] [--component <id>] [--rig <id>] [--status <status>] [--since 24h] [--artifact-kind <kind>] [--aggregate-artifact-kind <kind>]
homeboy runs artifact get <run-id> <artifact-id> [--output <path>]
homeboy runs artifact cleanup-downloads [--runner <runner-id>] [--run-id <run-id>] [--apply]
homeboy runs artifact cleanup-persisted [--older-than-days <days>] [--run-id <run-id>] [--apply]
homeboy runs export --run <run-id> --output <dir>
homeboy runs export --since <duration> --output <dir>
homeboy runs import <dir>
homeboy runs import --from-gh-actions --component <id> --repo <owner/repo> --workflow <workflow.yml> --artifact-glob <glob>
homeboy runs import --from-gh-actions --component <id> --repo <owner/repo> --run-id <gh-run-id> --artifact-glob <glob>
```

## Description

`homeboy runs` is the inspection and maintenance surface for Homeboy's local observation store. Producers such as `bench`, `rig`, and `trace` write run and artifact records; this command lets humans and agents inspect that evidence without opening SQLite directly, export/import portable bundles, and run explicit cleanup or reconciliation tasks.

`homeboy runs list` reads only the local observation store by default. Pass `--include-active-runner-jobs` to also append active jobs from connected runner daemons, which may inspect runner sessions. `homeboy runs list --runner <runner-id>` queries a connected runner daemon instead of the local observation store, preserving the normal `runs.list` JSON payload while returning evidence from the runner machine.

The JSON output includes stable run fields: run id, kind, status, timestamps, component id, rig id, git SHA, command, cwd, metadata, and artifact records where relevant.

`homeboy runs show <run-id>` prints a compact human summary by default: run identity, status, component/rig/SHA, timestamps, and each recorded artifact's locator with a concise `homeboy runs artifact get <run-id> <artifact-id>` command to inspect it. This makes bench artifacts (shared-state files, WP Codebox bundles, scenario outputs) easy to find without spelunking temp directories. Pass `--json` for the full structured payload on stdout; it is also always written to `--output <file>`.

`homeboy runs refs` emits a compact machine-readable ref index for matching runs. It is intended for matrix orchestration scripts and agents that need stable run refs and aggregate artifact refs without scraping human stdout. The output includes `homeboy://run/<id>` refs, `homeboy://run/<id>/artifact/<artifact-id>` refs, evidence/artifact follow-up commands, and detected aggregate artifact refs. Aggregate detection is schema-blind by default (`aggregate` in artifact id/kind/path); pass `--aggregate-artifact-kind <kind>` to mark additional artifact kinds as aggregate outputs.

```bash
homeboy --output json runs refs --kind bench --component studio --rig studio-bfb --since 24h
homeboy --output json runs refs --kind trace --component gutenberg --aggregate-artifact-kind trace_summary
```

`homeboy runs resume-plan <run-id>` reads generic `validation_progress` metadata from a run and reports the last completed command, any active command, and the next pending command. Homeboy core records this ledger for Homeboy-managed validation command sets without understanding npm, smoke groups, benchmarks, or implementation-specific command names; command manifests come from project configuration or extension-provided runners.

`homeboy runs evidence <run-id>` emits reviewer-facing evidence using generic artifact addresses. Local operator files are represented as non-reviewer-visible `homeboy://run/<run-id>/artifact/<artifact-id>` handles with a fetch command instead of absolute machine paths. Remote runner artifacts use `runner-artifact://...` refs, validated public HTTP(S) URLs are emitted as public evidence links, and metadata-only evidence remains non-public. Lab-specific publication or mirroring policy belongs in runner/extension enrichment, not in the generic evidence serializer.

`homeboy runs artifact cleanup-downloads` plans cleanup for local runner artifact downloads under Homeboy's artifact root (`<artifact-root>/runner`). By default it is a dry run; pass `--apply` to remove the planned cache subtree. Use `--runner` and `--run-id` to narrow cleanup to a specific runner or run cache.

`homeboy runs artifact cleanup-persisted` plans cleanup for persisted local run artifacts and their database records. By default it is a dry run; pass `--apply` to delete planned artifact files/directories and remove their database rows.

`homeboy runs reconcile` marks orphaned `running` observation records stale. Treat it as a mutating maintenance command, not a reader.

`homeboy runs distribution` aggregates categorical values from dot-separated JSON metadata paths. Scalar string, number, and boolean values are counted directly; arrays are flattened and counted by scalar element. The output reports inspected runs, matched/missing runs per field, total and unique value counts, value percentages, and repeated values.

## Compare Metrics Across History

`homeboy runs compare` compares selected persisted metrics across recent observation runs. It defaults to benchmark history and the `total_elapsed_ms` metric:

```bash
homeboy runs compare --kind bench --component studio --metric total_elapsed_ms --limit 20
homeboy runs compare --kind bench --component studio --rig studio-bfb --scenario studio-agent-site-build --metric total_elapsed_ms --metric p95_ms
```

The default output is a Markdown table with run id, status, start time, git SHA, rig id, artifact count, scenario, and selected metric columns. Use `--format=json` for structured output, or pair it with global `--output <file>` to write command JSON to disk:

```bash
homeboy runs compare --kind bench --component studio --metric total_elapsed_ms --format=json --output runs-compare.json
```

Metric lookup supports top-level run metadata such as `results.total_elapsed_ms`, direct dotted paths, and benchmark scenario metrics recorded under `scenario_metrics[].metrics` or `metric_groups`.

## Related Readers

```bash
homeboy bench history <component> [--scenario <id>] [--rig <id>] [--limit 20]
homeboy bench distribution <component> --field <metadata.path> [--scenario <id>] [--rig <id>] [--status <status>] [--limit 20]
homeboy bench compare --from-run <run-id> --to-run <run-id>
homeboy rig runs <id> [--limit 20]
```

These commands are thin read-only wrappers over the same observation-store records.

## Portable Bundles

`homeboy runs export` writes an inspectable directory bundle for moving observation evidence between machines without copying raw SQLite:

```text
homeboy-observations/
  manifest.json
  runs.json
  artifacts.json
  trace_spans.json
  findings.json
  test_failures.json
```

The v1 bundle is metadata-only: artifact records are exported, but artifact file bytes are not copied. Imported local file and directory artifacts are stored as `metadata-only` records with portable labels rather than source-machine paths. `homeboy runs query` reports these rows as skipped evidence, and `homeboy runs artifact get` explains that bytes are unavailable. `findings.json` contains normalized observation findings, and `test_failures.json` is an additive subset of findings where test commands recorded individual failures. Zip output is intentionally out of scope for v1; pass a directory path to `--output`.

`homeboy runs import` is idempotent. Existing identical records are accepted, while conflicting records with the same primary key fail clearly.

`homeboy runs export` writes a directory bundle. `homeboy runs import` mutates the local observation store by inserting the bundle's records when they are new or identical.

## GitHub Actions Artifacts

`homeboy runs import --from-gh-actions` imports JSON files from matching GitHub Actions artifacts into the local observation store. Use `--workflow` to scan recent workflow runs, or `--run-id` when triage starts from an exact GitHub Actions run URL or ID and the workflow filename is irrelevant.

The structured output includes stable Homeboy run/artifact IDs and persisted local artifact paths under `artifacts[]`, so agents can read the copied JSON directly without searching temporary download directories.

## Mutating Subcommands

Most `homeboy runs` subcommands are readers. These subcommands write files,
delete files, or update the local observation store:

- `artifact get`: copies a recorded file artifact to a local destination.
- `artifact cleanup-downloads --apply`: deletes locally cached runner artifact downloads.
- `artifact cleanup-persisted --apply`: deletes persisted local artifact files/directories and their database records.
- `export`: writes an observation bundle directory.
- `import`: inserts observation bundle or GitHub Actions artifact records into the local observation store.
- `loop-sync`: syncs continuous-loop archive directories into observation artifacts.
- `reconcile`: marks orphaned running records stale.

```bash
homeboy runs import --from-gh-actions \
  --component wp-site-generator \
  --repo example-org/wp-site-generator \
  --run-id 26731420339 \
  --artifact-glob 'php-transformer-iterator-transcript-*'
```
