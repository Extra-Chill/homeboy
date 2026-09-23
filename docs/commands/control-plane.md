# `homeboy control-plane`

Reconcile authoritative control-plane provider effects.

## Synopsis

```sh
homeboy control-plane <COMMAND>
```

## Subcommands

- `provider-effects` — reconcile deployment-provider effect state from authoritative evidence

## `provider-effects reconcile`

Terminalize one ambiguous effect without invoking its provider again.

A provider effect becomes ambiguous when Homeboy cannot tell whether the
provider applied it — for example, when the process that requested it stopped
before recording the outcome. Retrying the provider could apply the effect a
second time. `reconcile` resolves the ambiguity from evidence instead: given
authoritative proof of what the provider actually did, it records that result
as the effect's terminal state, and never calls the provider again.

```sh
homeboy control-plane provider-effects reconcile \
  --effect-id <EFFECT_ID> \
  --request-digest <REQUEST_DIGEST> \
  --recovery-fence <RECOVERY_FENCE> \
  --terminal-result '{"exit_code":0,"evidence":{}}' \
  --authoritative-evidence '<JSON>' \
  --apply
```

Arguments:

- `--effect-id` — canonical control-plane effect ID
- `--request-digest` — immutable digest from the original provider-effect
  request; binds the reconciliation to the exact request that became ambiguous
- `--recovery-fence` — recovery lease fence observed with the ambiguous
  effect; a reconciliation under a stale fence is rejected
- `--terminal-result` — terminal provider result JSON
- `--authoritative-evidence` — provider evidence JSON proving that result

Without `--apply` the command reports the plan only and never mutates;
`--dry-run` requests that default explicitly.
