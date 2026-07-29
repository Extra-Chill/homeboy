# Required CI Gates

`main` is protected by the versioned ruleset payload in
[`../../.github/required-gates-ruleset.json`](../../.github/required-gates-ruleset.json).
Every listed context must be emitted by `.github/workflows/ci.yml` on every pull
request. `homeboy / Required Gates Policy` runs the local validator; a renamed,
removed, duplicate, or path-filtered required job fails before it can weaken the
declared contract.

## Apply And Verify

The GitHub ruleset is repository state, so it cannot be changed by a pull
request. After this change merges, a repository administrator configures the
`main-ruleset-administration` GitHub environment with required reviewers and an
environment secret named `HOMEBOY_RULESET_ADMIN_TOKEN`. That token is a
repository-scoped GitHub App installation token or fine-grained token with only
repository-administration read/write access. The dispatch workflow has only
`contents: read`; the administration token is available solely to its approved
apply job.

Run the unprivileged preflight first, inspect its `required-gates-preflight`
artifact, then dispatch the approved apply operation from `main`:

```bash
gh workflow run required-gates-ruleset.yml --repo Extra-Chill/homeboy --ref main \
  -f operation=dry-run
gh workflow run required-gates-ruleset.yml --repo Extra-Chill/homeboy --ref main \
  -f operation=apply \
  -f confirmation=APPLY_REQUIRED_GATES
```

The `required-gates-apply` artifact records the immutable workflow inputs and
the before, desired, and after ruleset documents. The workflow fails unless the
after document exactly matches the checked-in policy. Its
`required_status_checks` rule contains exactly the eight contexts in the payload
and sets `strict_required_status_checks_policy` to `true`. Test the installation
with a PR that leaves `homeboy / Test`, `homeboy / Lint`, or `homeboy / Audit`
pending; GitHub must report the PR as blocked until the check succeeds.

## Emergency Path

Normal emergency work still uses a PR and waits for this gate set. If an active
incident requires a merge before a gate can finish, the repository owner records
an open issue titled `Emergency CI bypass: <PR number>` before dispatching the
approved bypass. The issue includes the PR URL, immutable head SHA, incident and
rollback plan, each outstanding check with its URL and state, and the owner
approving the bypass.

```bash
gh workflow run required-gates-ruleset.yml --repo Extra-Chill/homeboy --ref main \
  -f operation=emergency-bypass \
  -f confirmation=EMERGENCY_BYPASS_REQUIRED_GATES \
  -f emergency_issue=<issue-number>
```

The workflow requires the open issue and exact confirmation, removes only the
versioned required-status-check rule, and records the before/desired/after state
in `required-gates-apply`. Restore the normal policy through the approved
`operation=apply` dispatch immediately after the incident merge.

The owner attaches the workflow artifact URLs and post-merge check outcomes to
the issue, then closes it. The current ruleset has no bypass actors; the required
environment approval makes every exception a visible, time-bounded
repository-admin action in GitHub's audit trail.
