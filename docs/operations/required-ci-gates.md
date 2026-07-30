# Required CI Gates

`main` is protected by the versioned ruleset payload in
[`../../.github/required-gates-ruleset.json`](../../.github/required-gates-ruleset.json).
Every listed context must be emitted by `.github/workflows/ci.yml` on every pull
request. `homeboy / Required Gates Policy` runs the local validator; a renamed,
removed, duplicate, or path-filtered required job fails before it can weaken the
declared contract.

## Apply And Verify

The GitHub ruleset is repository state, so it cannot be changed by a pull
request. After this change merges, a repository administrator applies the
versioned payload to the existing `main` ruleset and verifies the result:

```bash
gh api --method PUT repos/Extra-Chill/homeboy/rulesets/13680120 \
  --input .github/required-gates-ruleset.json
bash .github/validate-required-gates.sh --github
gh api repos/Extra-Chill/homeboy/rulesets/13680120
```

The final command is review evidence. Its `required_status_checks` rule must
contain exactly the seven contexts in the payload and set
`strict_required_status_checks_policy` to `true`. Test the installation with a
PR that leaves `homeboy / Test`, `homeboy / Lint`, or `homeboy / Audit` pending;
GitHub must report the PR as blocked until the check succeeds.

## Emergency Path

Normal emergency work still uses a PR and waits for this gate set. If an active
incident requires a merge before a gate can finish, the repository owner records
an issue titled `Emergency CI bypass: <PR number>` before changing the ruleset.
The issue includes the PR URL, immutable head SHA, incident and rollback plan,
each outstanding check with its URL and state, and the owner approving the
bypass. The owner changes the rule only through the GitHub ruleset UI, performs
the merge, then restores the checked-in payload with the apply command above.

The owner attaches the `--github` verification output and post-merge check
outcomes to the issue, then closes it. The current ruleset has no bypass actors;
keeping bypass access out of the standing policy makes every exception a visible,
time-bounded repository-admin action in GitHub's audit trail.
