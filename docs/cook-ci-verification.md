# CI-deferred Cook verification (first slice)

Use `homeboy agent-task cook ... --verify 'cargo fmt --all --check' --verify-ci`.
The explicit local gates still have to pass; no full-suite gate needs to be
supplied. CI mode requires PR publication (not `--no-finalize`).

After publication the Cook reports `awaiting_ci`. `agent-task status COOK_ID`
reads its durable CI receipt without network access. `agent-task recover COOK_ID`
re-polls GitHub and persists the result; repeat while checks are pending.
Recovery preflight does not poll. Branch protection and branch rules determine
required check names when readable; otherwise every reported check matters.
Missing required checks and an empty check set remain pending. Evidence is bound
to the PR head SHA, and a concurrent head change refuses the poll.

Green checks produce `completed` with verified evidence. Red checks produce
`ci_gate_failed`, with names and log URLs in `evidence.feedback`. Cancellation,
timeout, and startup failure produce `ci_infrastructure`, not candidate feedback.
Transport failures leave the last receipt untouched.

This slice uses explicit recovery polling, not a background subscription.
Automatic retry dispatch and pushing remediation attempts to the same PR branch,
notifications, log excerpts, and integration of CI receipts into all dossier and
control-plane projections are follow-ups. The receipt is not a replacement for
pre-publication local gate evidence. CI supplies baseline/candidate attribution;
Homeboy does not infer it from job names.
