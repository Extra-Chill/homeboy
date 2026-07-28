# Structured Evidence

Homeboy treats evidence as a first-class output, not as terminal text someone has to copy into a comment.

## Human Output

Commands print readable summaries to stdout so local developers can act quickly.

## JSON Output

Most commands can also write structured JSON:

```bash
homeboy review --changed-since origin/main --output homeboy-results/review.json
```

CI jobs, scheduled automation, and coding agents should read this JSON instead of scraping stdout.

## Persisted Runs

Observation-heavy workflows can persist run records and artifacts. Inspect them with:

```bash
homeboy runs list
homeboy runs show <run-id>
homeboy runs artifacts <run-id>
homeboy runs evidence <run-id>
```

## Interpretation

Artifacts are facts; they do not say what the facts mean. The evidence manifest
(`homeboy/evidence-manifest/v1`) is the interpretation layer: a state, a summary,
a confidence grade, blocking conditions, and portable references to the trackers,
pull requests, runs, and artifacts the judgement rests on.

`homeboy runs evidence <run-id>` always emits one. A producer that knows more
than the exit code can attach its own — at `metadata.evidence_manifest`, or as an
artifact of kind `evidence_manifest` — and Homeboy surfaces it verbatim.
Otherwise Homeboy derives one from the run record and marks it `source: derived`,
so an authored assertion is never confused with a mechanical reading.

```bash
homeboy contract show evidence-manifest
homeboy contract validate homeboy/evidence-manifest/v1 --file manifest.json
```

## Reviewer-Safe Evidence

Reviewer-facing evidence should point to a reachable artifact, PR comment, issue, release asset, or exported run bundle. Local paths and localhost URLs are operator notes, not durable review evidence.

## Reference

- [JSON output contract](../architecture/output-system.md)
- [CI result JSON contract](../architecture/ci-results-contract.md)
- [Persisted runs](../commands/runs.md)
