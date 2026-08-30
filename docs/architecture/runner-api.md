# Runner API

Homeboy's Runner API is the core-owned machine-execution boundary. It accepts a
transport-neutral execution envelope and returns a canonical execution record.
The API does not own workflow policy or machine transport mechanics.

## Ownership

- `homeboy-runner-contract` owns behavior-free execution envelopes and records.
- `homeboy-core::runner` owns runner service dispatch.
- `homeboy-lab-runner` implements local, daemon, SSH, and reverse-broker execution.
- The control-plane API coordinates lifecycle using execution identities.
- The extension API contributes capabilities and workload definitions.

These are peer APIs. The control plane and extensions may consume runner
contracts, but the Runner API does not depend on control-plane resources or
extension manifests.

## Initial Surface

The first vertical slice exposes one operation:

```text
submit(RunnerExecutionEnvelope) -> RunnerExecutionRecord
```

Core fails closed when no runner implementation is registered. Product
composition registers the Lab runner adapter at startup. The adapter translates
the canonical request once, then selects local, daemon, SSH, or reverse-broker
transport behind the service boundary.

Runner administration remains separate. Registry mutation, trust, pairing,
tunnels, diagnostics, binary refresh, caches, and workspace maintenance are not
part of execution submission.

## Next Operations

The same service will add capability discovery, placement preflight, execution
observation, event cursors, cancellation, reconciliation, leases, and artifacts.
Each operation must use versioned contracts and remain independent of transport.
