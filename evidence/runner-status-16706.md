# Runner status retirement verdict measurement

Measured on the controller against `homeboy-lab`, 2026-10-30.
A PATH-local `ssh` executable logged arguments and then exec'd `/usr/bin/ssh`;
no remote command or runner state was changed by the measurement.

| Binary | Wall time | SSH calls | Retained-evidence SSH calls |
| --- | ---: | ---: | ---: |
| Installed binary, before | 45.88 s | 175 | 172 |
| Workspace binary, after | 1.99 s | 2 | 0 |

Both measurements ran `runner status homeboy-lab`. Evidence calls were
identified by `/runs/` paths in the SSH arguments. The changed status reads
only the last persisted retirement verdict; it does not refresh that proof.

The CLI regression exercises both unqualified and runner-specific status with
a draining generation and a terminal retained run. Its HTTP fixture records
requests and rejects any evidence request. Generation-store assertions cover
timestamped evidence blockers, their run IDs, unchanged persisted decisions
after local evidence changes, and successful retirement verdicts.
