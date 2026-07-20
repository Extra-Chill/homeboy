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

`homeboy deploy --project <project> --release-set <path> --dry-run` performs registry lookup, clean-Git-checkout validation, and exact-ref inspection for every available component before deployment mutates a remote target. Each available component is materialized and deployed from its own manifest ref; dry-run output includes the normalized release-set identity plus every component's requested ref and resolved SHA. `--ref` is excluded because the manifest owns those source identities.

Release-set deploy currently accepts one `--project` target at a time. A failure before deployment leaves source checkouts and remote targets unchanged. After deployment starts, Homeboy retains its existing per-component failure reporting; transfer failures are not a transactional rollback protocol. Multi-project/fleet/shared release sets, runtime comparison, and rollback orchestration remain follow-up work.
