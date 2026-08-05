# Required CI Gates

Two different things can be true or false here, and conflating them is what
caused #11084:

- **Declaration** — every context named by
  [`../../.github/required-gates-ruleset.json`](../../.github/required-gates-ruleset.json)
  is emitted by `.github/workflows/ci.yml` on every pull request. This is
  repository *content*, so a pull request can break it and a pull request can
  fix it.
- **Enforcement** — GitHub actually requires those contexts before `main` can be
  updated. This is repository *state*. A pull request cannot change it, and it
  can be false while the declaration is perfect.

`homeboy / Required Gates Declaration` runs
`bash .github/validate-required-gates.sh --report`. It **fails closed on the
declaration** — a renamed, removed, duplicate, or path-filtered required job
turns the check red — and it **reports, but never enforces, the live
enforcement outcome**. Nothing in this check can newly block a pull request.

## What The Check Reports

Every run emits one machine-readable provenance line plus a job summary
recording the target branch, ruleset id, head SHA, declared contexts, live
required contexts, strict setting, and bypass actors:

```
::notice::required-gates enforcement basis=live-branch-rules repo=… branch=main
  ruleset=… head=… declared=7 live=0 rules=0 strict=false bypass_actors=0
  current_user_can_bypass=never outcome=unenforced
```

| outcome | meaning |
| --- | --- |
| `enforced` | live required contexts equal the declared set, strict policy on |
| `bypassable` | as `enforced`, but actors can bypass the ruleset — a bypassing merge is not gated |
| `divergent` | a required-status-checks rule exists and disagrees with the payload |
| `unenforced` | live state is readable and requires **no** checks at all |
| `unverified` | live state could not be read; enforcement is unproven, never assumed |

Anything other than `enforced` is a loud `::warning::` annotation naming the
exact GitHub setting that must change. `unverified` is deliberately its own
outcome: the live read needs a token that can read repository rules, and a
failure to read must never be reported as a pass.

Prior to #11084 this job was named `homeboy / Required Gates Policy` and ran
`--local`, so a green tick asserted enforcement it had never checked. On
2026-08-01, PR #11069 merged nine minutes ahead of a red `homeboy / Test` under
that green tick, because `repos/Extra-Chill/homeboy/rules/branches/main` carried
no required-status-check rule at all.

This is a reporting fix on purpose. The repository merges fast by design and a
post-merge guard was removed by design; the defect was the false assurance, not
the absence of a block.

## Apply And Verify

The GitHub ruleset is repository state, so it cannot be changed by a pull
request. A repository administrator applies the versioned payload to the
existing `main` ruleset and verifies the result:

```bash
gh api --method PUT repos/Extra-Chill/homeboy/rulesets/13680120 \
  --input .github/required-gates-ruleset.json
bash .github/validate-required-gates.sh --github
gh api repos/Extra-Chill/homeboy/rulesets/13680120
```

`--github` is the administrator verification path and is the one mode that
**fails closed on enforcement** — `divergent`, `unenforced`, and `unverified`
all exit non-zero. It is deliberately not what CI runs.

The final command is review evidence. Its `required_status_checks` rule must
contain exactly the seven contexts in the payload and set
`strict_required_status_checks_policy` to `true`. Test the installation with a
PR that leaves `homeboy / Test`, `homeboy / Lint`, or `homeboy / Audit` pending;
GitHub must report the PR as blocked until the check succeeds. Until that apply
happens, the CI job reports `unenforced` on every pull request, which is the
honest answer rather than a silent green tick.

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
time-bounded repository-admin action in GitHub's audit trail. If bypass actors
are ever added intentionally, the check reports `bypassable` rather than
`enforced`, and names both the actor count and whether the current actor can
bypass, so the exception stays visible on every pull request.
