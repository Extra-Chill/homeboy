# PR report routing

`pr-comment` defaults to `on-new-failures`. `off` disables comment writes;
`always` preserves explicit status-report comments (`false`/`true` are legacy
aliases). The rich report and tooling footer go to `GITHUB_STEP_SUMMARY` in all
modes. Inline annotations are independent of comment mode.

Lint attribution subtracts baseline `lint_findings` fingerprints. If no baseline
finding set is available, the action uses findings in PR-changed files (GitHub's
paginated files API); API failure yields no attributed findings. Counts alone can
report a definite new failure but cannot manufacture locations. Tests use the
gate's validated candidate-only identities, after retry/flaky exclusions.
Inconclusive evidence does not create or resolve comments in default mode.

Each component retains its own section in the shared sticky comment, including
when resolving: a green component must not erase a different component's red
section. A clean section is replaced with `✅ resolved — no new findings` only
when a shared sticky comment already exists. Posting continues through the core
atomic expected-head guard and matrix section merge primitive.

Comments cap located findings at ten per component; workflow annotations cap
at fifty per action invocation, errors before warnings. Messages and properties
are mention-neutralized and workflow-command escaped independently.
