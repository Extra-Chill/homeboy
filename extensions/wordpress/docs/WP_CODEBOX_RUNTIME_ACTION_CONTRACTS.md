# WP Codebox Runtime Action Contracts

Homeboy's WordPress fuzz runtime workload operation descriptor uses schema
`homeboy/wordpress-fuzz-runtime-workload-operation/v1` to describe WordPress
actions such as REST requests, CRUD operations, admin/frontend page loads, block
render/editor operations, database queries, WP-CLI calls, login/nonce helpers,
checkpoint/restore/reset state, replay, and minimization.

The descriptor maps to WP Codebox only through an explicit public runtime action
contract supplied by WP Codebox. Homeboy Extensions does not infer WP Codebox CLI
commands, ability names, request schemas, or response schemas from product
knowledge.

## Contract Input

The mapper accepts a public contract object with action descriptors under one of
these fields: `actions`, `runtime_actions`, `wordpress_runtime_actions`,
`operations`, or `workload_operations`.

Each action descriptor may declare:

- `schema`
- `action`
- `ability`
- `command`
- `input_schema` or `inputSchema` or `request_schema` or `requestSchema`
- `output_schema` or `outputSchema` or `result_schema` or `resultSchema`

Example fixture shape:

```json
{
  "schema": "wp-codebox/wordpress-runtime-action-contracts/v1",
  "actions": {
    "rest_request": {
      "schema": "wp-codebox/wordpress-runtime-action/v1",
      "action": "rest_request",
      "ability": "wp-codebox/runtime-action/rest_request",
      "input_schema": "wp-codebox/wordpress-runtime-action/rest_request/input/v1",
      "output_schema": "wp-codebox/wordpress-runtime-action/rest_request/output/v1"
    }
  }
}
```

## Blocker Behavior

When no public WP Codebox action contract is supplied, Homeboy emits a blocked
runtime operation with blocker code
`wp-codebox-runtime-action-contracts-missing`.

When a contract is supplied but does not declare the requested action, Homeboy
emits blocker code `unsupported-wordpress-runtime-action` and includes the
contract-declared `supported_actions` list.

These blockers are intentional production behavior. They preserve the boundary
that Homeboy consumes Codebox public contracts only, and they prevent accidental
coupling to private Codebox commands, probes, or fallback shims.
