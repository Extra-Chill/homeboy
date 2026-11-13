# Cook gate runtime admission (#18018)

## Behaviour
- `--verify` and private gate programs are scanned conservatively for literal, bare command names: the first non-assignment word of each simple command. Builtins, keywords, expansions (`$`, backticks, globs) and path-style commands (`./scripts/check.sh`, `vendor/bin/phpunit`) are skipped; the candidate may create path-style commands.
- Each inferred name must resolve on the PATH of the environment the gate will run in. Locally that is the selected gate environment. On a Lab runner it is the runner's authenticated, non-executing readiness receipt. Nothing is executed.
- A missing executable rejects the Cook at preview or admission with `gate_runtime_unavailable` (`gate`, `executable`, `runner`, `next_action`). The code survives the Lab readiness receipt boundary.

## Lab check, 2026-11-12
On `homeboy-lab`, `php` resolves to `/usr/bin/php` (`/etc/alternatives/php`) in a login shell and under `env -i`, so it is on the runner's system PATH. The reported "sandbox has no php" failure of `agent-task-03a51cce-…` does not reproduce from the runner's PATH. That run's durable diagnosis records a different pre-execution blocker (`runner exec attempt … lab-hydration-0 is already terminal`). If a gate sandbox genuinely lacks a runtime, admission now reports it before the agent runs.

## Out of scope
Inferring runtimes behind `homeboy review <kind> <component>` gates needs extension-declared gate runtimes. The existing extension `runtime` block also drives `component env` resolution, and WordPress tests can run on Playground without host PHP, so declaring host `php` there would reject valid gates.
