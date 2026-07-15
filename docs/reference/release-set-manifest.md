# Release Set Manifest

A release set is a caller-supplied, product-agnostic source-membership contract. It is versioned by `schema` and has a deterministic SHA-256 identity after components are sorted by ID.

```json
{
  "schema": "homeboy/release-set/v1",
  "components": [
    { "id": "component-a", "ref": "0123456789012345678901234567890123456789" },
    { "id": "component-b", "ref": "0123456789012345678901234567890123456789", "required": false }
  ]
}
```

`id` and `ref` are required. `required` defaults to `true`. IDs must be unique. Missing required components fail preflight; unavailable optional components are omitted from deployment. Consumers can compare the normalized set with observations and receive deterministic `missing`, `unexpected`, and `mismatched` lists.

`homeboy deploy --project <project> --release-set <path> --dry-run` performs the same registry lookup, clean-Git-checkout, and exact-ref resolution that deployment uses. A failed proof prevents lifecycle creation, builds, transfers, and remote deployment. This first deploy vertical requires every component to share one exact ref; broader multi-ref orchestration is intentionally not implied by the generic contract.
