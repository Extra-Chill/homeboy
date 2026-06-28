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

## Reviewer-Safe Evidence

Reviewer-facing evidence should point to a reachable artifact, PR comment, issue, release asset, or exported run bundle. Local paths and localhost URLs are operator notes, not durable review evidence.

## Reference

- [JSON output contract](../architecture/output-system.md)
- [CI result JSON contract](../architecture/ci-results-contract.md)
- [Persisted runs](../commands/runs.md)
