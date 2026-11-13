# Runner wedge repair evidence

Closes #18013.

## Before / after

- Before (Lab 0.459.1, incident root-cause comment): approximately 18 ledger opens/second attributed to the health updater; all connection workers died following store mutex poisoning.
- After (local strace, focused `repeated_store_reload_reuses_connection` fixture): one `connection-reuse.sqlite` open for the first writer, no further ledger opens across 100 durable reload/commit cycles. Thus steady-state opens from this store reload/read path are 0/second (initialization excluded). This is a focused store measurement, not a production Lab CPU or whole-daemon measurement. Independent path-only inspection APIs and other observation subsystems can still open their own connections.
- Freshness refresh is bounded to once/second under revision churn and once/five seconds when idle. The health endpoint continues to serve the cached snapshot, without waiting for freshness computation.

## Contracts

Resolution runs outside the store mutex. Reconciliation rechecks existence and active status after the authoritative transaction reload; vanished or terminal candidates are skipped. The remaining job-existence assertions in transaction closures were audited: selection, insertion, or explicit validation happens inside the same transaction; the attestation lookup was also made tolerant of absent live-mirror rows.

Only connection workers retain queue receivers. Disconnected dispatch returns an error and closes the accepted socket. Unbounded daemon serve enables a fatal panic hook for all daemon threads; bounded/unit fixtures do not enable process exit. A separate TCP self-health probe restarts after twelve consecutive failures (about two minutes at a ten-second cadence, five-second socket timeout), so handlers blocked on the admission fence never restart a busy daemon.

The persistence owns a lazily initialized mutex-protected SQLite connection, shared by cloned stores. Existing process/advisory writer locks and immediate commit transactions are unchanged. Repeated read statements are cached. Path-only inspection remains independent and read-only.

## Compatibility / AI disclosure

Additive daemon resilience; no wire or durable schema changes. On a daemon thread panic, process exit replaces silently continuing with a broken serving path.

AI assistance: OpenAI model exposed by Pi as `cook-heavy`, using Pi coding tools to inspect the incident, implement changes, run tests and collect local strace evidence. The underlying model identifier was not exposed by the harness.
