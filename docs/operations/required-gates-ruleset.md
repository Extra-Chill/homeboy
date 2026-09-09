# Required Gates Ruleset

`.github/required-gates-ruleset.json` is the reviewed candidate for the active
`main` ruleset `13680120`. It requires strict checks for the PR head, including
`homeboy / Required Gates Executed`, which fails whenever Rustfmt, Lint, or Test
is pending, skipped, cancelled, failed, or absent from the terminal job's
dependencies.

The scheduled `Required Gates Ruleset Audit` workflow is read-only. It queries
the live ruleset and fails with reviewer-resolvable evidence containing the
repository, branch, ruleset ID, head SHA, expected and live contexts,
strictness, and bypass actors.

## Operator Application

After this candidate is reviewed and merged, an administrator must apply it to
the existing live ruleset. This repository change deliberately does not mutate
GitHub configuration:

```sh
gh api --method PUT repos/Extra-Chill/homeboy/rulesets/13680120 \
  --input .github/required-gates-ruleset.json
```

Then manually run `Required Gates Ruleset Audit` and retain its successful run
URL as the application evidence.
