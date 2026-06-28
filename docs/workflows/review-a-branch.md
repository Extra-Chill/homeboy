# Review A Branch

Use `homeboy review` when you need the PR-shaped quality gate for a branch. It runs scoped audit, lint, and test checks against the same changed-file set and reports one consolidated result.

## Use This When

- You want to know whether a branch is review-ready.
- You need one proof artifact instead of separate audit, lint, and test logs.
- A coding agent needs a stable JSON result to inspect.
- A PR comment should summarize the branch status for reviewers.

## 1. Pick The Base Ref

Scope the review to what a reviewer will see. For most branches, that is the default branch:

```bash
homeboy review --changed-since origin/main
```

Use another base when the branch is stacked or cut from a release tag:

```bash
homeboy review my-component --changed-since origin/trunk
homeboy review my-component --changed-since v1.2.0
```

Use `--changed-only` only for a quick local check of the current working tree. Use `--changed-since` for reviewer-facing proof.

## 2. Capture The Result

For a human terminal run:

```bash
homeboy review my-component --changed-since origin/main
```

For CI or agent handoff, write the structured envelope:

```bash
homeboy --output homeboy-results/review.json \
  review my-component --changed-since origin/main --summary
```

For a PR-comment body:

```bash
homeboy review my-component \
  --changed-since origin/main \
  --report pr-comment > homeboy-results/review-comment.md
```

## 3. Read Failures

Treat `review` as the summary layer. When one stage fails, deep dive with the same scope:

```bash
homeboy audit my-component --changed-since origin/main
homeboy lint my-component --changed-since origin/main
homeboy test my-component --changed-since origin/main
```

The goal is to keep the final reviewer-facing proof as `homeboy review`, while using individual stages for diagnosis.

## 4. Add A Declared CI Profile When Needed

If the component declares a CI profile, include it as an additional review stage:

```bash
homeboy review my-component --changed-since origin/main --ci-profile pr
```

`review --ci-profile` runs declared Homeboy CI profiles. It does not attempt to interpret arbitrary provider YAML.

## 5. Use Runner Proof For Hot Gates

For release-gate proof, use normal routing and avoid local-hot bypasses:

```bash
homeboy --runner <runner-id> review my-component --changed-since origin/main
```

See [Release-gate proof path](../operations/release-gate-proof-path.md) when the proof must be non-local and reviewer-safe.

## Expected Outputs

- Terminal summary for humans.
- Optional JSON envelope at the path passed to `--output`.
- Optional Markdown PR-comment section when `--report pr-comment` is used.
- Stage-specific deep-dive commands when a failure needs investigation.

## Reference

- [review command](../commands/review.md)
- [audit command](../commands/audit.md)
- [lint command](../commands/lint.md)
- [test command](../commands/test.md)
- [CI reproduction workflow](reproduce-ci.md)
