# Review A Branch

Use `homeboy review` when you need the PR-shaped quality gate for a branch. It runs scoped audit, lint, and test checks against the same changed-file set and reports one consolidated result.

## Canonical Command

```bash
homeboy review --changed-since origin/main
```

For agent or CI handoff, write structured output:

```bash
homeboy review --changed-since origin/main --output homeboy-results/review.json
```

## When To Use Individual Stages

Use individual stages for diagnosis, not as the final proof artifact:

```bash
homeboy audit --changed-since origin/main
homeboy lint --changed-only
homeboy test
```

## Runner Proof

For release-gate proof, use normal routing and avoid local-hot bypasses. See [Release-gate proof path](../operations/release-gate-proof-path.md).

## Reference

- [review command](../commands/review.md)
- [audit command](../commands/audit.md)
- [lint command](../commands/lint.md)
- [test command](../commands/test.md)
