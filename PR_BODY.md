## Summary
Cook previously stopped before provider execution when its managed destination needed repair, leaving operators to reconcile it manually. Admission now converges the recorded destination against the pinned base, preserves attributable earlier work, and gives specific recovery routes for work it cannot safely replace.

## What changed
- Clean destinations without unique history converge to the pinned base; open PRs route to the exact PR-URL Cook command, and unique commits route through PR creation and rework.
- Missing recorded branches are restored from a surviving remote head or the recorded base, without substituting a new caller-provided base.
- Earlier Cook changes are preserved before reset; unattributed changes are refused with named files and a stash recovery action.
- An unlocked driver lock allows takeover of an interrupted Cook's destination, including preservation of a nonterminal attempt's candidate.
- Transient fetch and SSL failures receive bounded backoff retries and infrastructure classification; real-worktree regressions cover admission and preservation behavior.

## Compatibility
Additive recovery behavior replaces generic destination-state refusals. Caller-owned checkouts retain snapshot semantics, authenticated promoted candidates retain their verified HEAD, and promotion code is unchanged.

## Used for
AI inspected the linked issues, implemented destination convergence and recovery, and exercised the regression gates.

Closes #16323
