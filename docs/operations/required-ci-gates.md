# Required CI Gates

> [!IMPORTANT]
> **The checked-in policy requires the eight status checks below.**
>
> The live `main` ruleset (`13680120`) is audited hourly against that policy.
> Audit is read-only and fails closed with a replayable before/desired/after
> artifact when the ruleset drifts. It never changes GitHub state.
>
> Reconciliation is a separate, explicit `workflow_dispatch` operation from
> `main`, protected by the `main-ruleset-administration` environment. It only
> updates the existing ruleset ID after the exact current `main` revision has a
> successful `homeboy / Test`; it then verifies the effective branch rule. The
> evidence records a policy version, source revision, and canonical policy
> fingerprint so an operator can replay the decision.


Three different things can be true or false here, and conflating them is what
caused #11084 and #12573:

- **Declaration** — every context named by
  [`../../.github/required-gates-ruleset.json`](../../.github/required-gates-ruleset.json)
  is emitted by `.github/workflows/ci.yml` on every pull request. This is
  repository *content*, so a pull request can break it and a pull request can
  fix it.
- **Enforcement** — GitHub actually requires those contexts before `main` can be
  updated. This is repository *state*. A pull request cannot change it, and it
  can be false while the declaration is perfect.
- **Execution** — the declared gates actually ran and passed in a given run.
  This is *run* state. It can be false while the declaration is perfect and the
  enforcement is installed, because a skipped or cancelled gate reports nothing
  at all.

`homeboy / Required Gates Declaration` runs
`bash .github/validate-required-gates.sh --report`. It **fails closed on the
declaration** — a renamed, removed, duplicate, or path-filtered required job
turns the check red — and it **reports, but never enforces, the live
enforcement outcome**. Nothing in this check can newly block a pull request.

`homeboy / Required Gates Executed` runs
`bash .github/ci-required-gates-executed.sh` after every gate, under
`if: always()`. It **fails closed on execution**, and it is the only check whose
green tick means work was actually done.

## What The Check Reports

Every run emits one machine-readable provenance line plus a job summary
recording the target branch, ruleset id, head SHA, declared contexts, live
required contexts, strict setting, and bypass actors:

```
::notice::required-gates enforcement basis=live-branch-rules repo=… branch=main
  ruleset=… head=… declared=8 live=0 rules=0 strict=false bypass_actors=0
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

This reporting check does not itself merge-protect a pull request. The active
ruleset must require these contexts; the audit and approved reconciliation path
keep that repository state converged with the reviewed declaration.

## Execution: What A Green Run Has To Mean

Every gate in `ci.yml` is conditional on `homeboy / PR State`, and a `skipped`
needs-dependency does not fail a GitHub Actions run. On 2026-08-15 PR #12567
merged on exactly that: run `31906427396` was cancelled with all seven gates
mid-flight, and the `pull_request.closed` run that cancelled it — `31906482704` —
skipped every gate and concluded **success**. Net verification for the merge was
`PR State` and `CI Capacity Evidence`. Nothing compiled, nothing was tested, and
`gh pr checks` simultaneously showed the superseded run's `cancelled` jobs as
`fail`, so the pull request read red and green at once (#12573).

`homeboy / Required Gates Executed` is the terminal gate for that. It measures
two independent things and fails unless both hold:

| direction | claim |
| --- | --- |
| dependency results | every gate job `ci.yml` names in its `needs` concluded `success`; `skipped` and `cancelled` are failures here, not silence |
| observed execution | every non-terminal declared context appears in this run's job list with conclusion `success`, read back from `actions/runs/<id>/jobs`; the terminal context is enforced by GitHub after this job succeeds |

| outcome | meaning |
| --- | --- |
| `executed` | every declared context ran and passed — the only outcome that exits 0 |
| `skipped` | at least one required gate did not execute at all |
| `failed` | the gates executed and at least one did not pass |
| `unverified` | this run's job list could not be read; an unmeasured run is not a verified one |

Capacity admission and the closure run's cancellation are deliberately
unchanged. Skipping stays possible; it stops being invisible. **A
`pull_request.closed` run is red here on purpose**: it is the last run on the
pull request and it is the run that read green in #12573, so exempting it would
leave the reported hole exactly where it was found. Read the job summary, not the
colour, to tell "this run cancelled an in-flight run and verified nothing" from
"a gate failed".

The wiring half of this is declaration, not execution, so
`validate-required-gates.sh` fails closed when `required-gates-executed` is
missing, is not `if: ${{ always() }}`, does not invoke the assertion, or omits
any PR-state-conditional gate job from its `needs`. Adding a gate to `ci.yml`
without wiring it into the terminal job turns `Required Gates Declaration` red.

## Apply And Verify

The GitHub ruleset is repository state, so it cannot be changed by a pull
request. `Required Gates Ruleset` audits hourly and supports an approved manual
reconciliation operation from `main`. It does not create another ruleset.
Reconciliation first requires a successful
`homeboy / Test` check on the current `main` SHA, so it cannot re-enable strict
required checks while the main suite is red. Configure the `main-ruleset-administration`
environment with required reviewers, administrator bypass disabled, and a
repository-administration token named `HOMEBOY_RULESET_ADMIN_TOKEN`; only the
approved manual reconciliation job can use that credential.

After reviewing the audit artifact, dispatch reconciliation from `main`:

```bash
gh workflow run required-gates-ruleset.yml --repo Extra-Chill/homeboy --ref main \
  -f operation=reconcile
```

The approved workflow is the only reconciliation path. The reconciliation job
first verifies that the environment has required reviewers and does not permit
administrator bypass. It then checks that its checkout remains the live `main`
tip, requires a successful `homeboy / Test` check for that exact SHA, and
repeats the tip check immediately before it can update the existing ruleset. It
records the before/desired/after documents and fails unless they exactly match,
then runs the same fail-closed effective-enforcement verification used below.

`--github` is the administrator verification path and is the one mode that
**fails closed on enforcement** — `divergent`, `unenforced`, and `unverified`
all exit non-zero. It is deliberately not what CI runs.

The final command is review evidence. Its `required_status_checks` rule must
contain exactly the eight contexts in the payload and set
`strict_required_status_checks_policy` to `true`. The terminal
`homeboy / Required Gates Executed` context is required because it fails when
the other gates are cancelled or skipped. Test the installation with a PR that
leaves it, `homeboy / Test`, `homeboy / Lint`, or `homeboy / Audit` pending;
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
