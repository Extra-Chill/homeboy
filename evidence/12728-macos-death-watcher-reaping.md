# #12728 — macOS controller death watcher reaping

Tracker: https://github.com/Extra-Chill/homeboy/issues/12728

Base: `3de73f82695b8a8943c5de5d5d63f0935f41195a`.

## Root cause

On non-Linux Unix, `ControllerChildGuard::prepare()` forks a sibling controller
death watcher before spawning the workload. The parent discarded `_guard_pid`.
Dropping the guard closed its pipes and let that watcher exit or exec its cleanup
shell, but no parent wait path consumed the watcher's exit status. Every completed
guarded command could therefore leave another direct zombie in a long-lived
controller or daemon. The observed local daemon owned 1,255 zombie children.

## Repair

Each successful watcher fork now starts an exact-PID blocking waiter in a named
thread. Pipe closure preserves the existing controller-death cleanup; releasing
the thread handle lets the waiter finish without blocking guard destruction.
`waitpid(watcher_pid, ...)` retries interrupted waits and cannot consume another
workload's exit status. Failure to start the waiter kills and reaps the exact
watcher before returning a preparation error, before any workload is spawned.

Linux execution-scoped supervisor behavior is unchanged by this platform-gated
repair. This evidence establishes the macOS leak, not every historical cause
reported under the broader tracker.

## Deterministic verification

Native macOS:

```sh
cargo test -p homeboy-engine-primitives -- --test-threads=4
```

Result: **277 passed, 0 failed, 1 ignored**. The ignored test is an explicitly
invoked subprocess fixture. New regressions execute sixteen real guarded
commands, verify their exit code, verify each watcher no longer exists and is no
longer waitable (`ESRCH` / `ECHILD`), and preserve an unrelated child's exit code
42. A failed workload spawn also proves watcher reaping and nonblocking drop.
Existing timeout, cancellation, owner-drop and controller-loss tests passed.

Linux Lab:

```sh
cargo test -p homeboy-engine-primitives -- --test-threads=1
```

Result: **284 passed, 0 failed, 3 ignored**. Persisted run:
`hb-12728-child-reaping-linux-serial`; runner job:
`2089e873-5c55-41b9-89eb-851942273baa`.

The initial four-thread Linux run failed two existing adopted-child tests whose
process-global before/after zombie sets changed while neighboring tests ran. The
same snapshot passed the entire serial suite. Initial run:
`hb-12728-child-reaping-linux`; runner job:
`84cbe282-59c1-4a65-8ca0-06a2617cdf13`.

Strict Clippy is blocked by existing warnings: `should_implement_trait` in
`homeboy-error`, and `needless_return` / `needless_borrows_for_generic_args` in
unchanged primitive code. Formatting and diff whitespace checks pass.

## Runtime boundary

The installed daemon was not replaced during verification: it reported four
active jobs, and `homeboy daemon recover --dry-run` reported a fresh lease with
no executable recovery plan. The fix prevents new watcher zombies in rebuilt
processes; already-abandoned children require retirement of their owning process.

Implementation and verification were performed directly in OpenCode by
OpenAI gpt-6.1-sol under operator authorization, outside Cook orchestration.
