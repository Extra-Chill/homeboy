# Composable Workload Evidence Contract

Homeboy ranks and compares generic evidence. Product and workload meaning stays
outside Homeboy core so new scenarios can compose without teaching core each
product's vocabulary.

```text
+----------------------+
| homeboy-rigs         |
| product/workload     |
| semantics            |
+----------+-----------+
           | declares workload, inputs, product gates, labels
           v
+----------------------+
| WP Codebox           |
| generic WordPress    |
| runtime artifacts    |
+----------+-----------+
           | emits recipes, observations, logs, files, status
           v
+----------------------+
| Homeboy Extensions   |
| WordPress runtime    |
| plumbing             |
+----------+-----------+
           | normalizes runtime output to Homeboy contracts
           v
+----------------------+
| Homeboy core         |
| generic evidence     |
| ranking/comparison   |
+----------------------+
```

## Ownership

- `homeboy-rigs` owns product and workload semantics: scenarios, matrices,
  fixture discovery, expected behavior, product gates, diagnostic labels, and
  repair attribution.
- WP Codebox owns generic WordPress sandbox execution and standard artifacts:
  recipes, action results, runtime status, logs, files, browser captures, and
  performance observations. It reports what happened without deciding product
  pass/fail meaning.
- Homeboy Extensions owns WordPress runtime plumbing: component mounting,
  dependency and blueprint setup, WP-CLI/browser helper dispatch, artifact lookup,
  and normalization into Homeboy's generic evidence contracts.
- Homeboy core owns durable orchestration, structured output, persisted runs,
  evidence comparison, generic ranking, and report rendering. Core treats product
  names, route groups, fixture ids, and diagnostic classes as opaque metadata.

## Contract

- Product-specific workload definitions live in rig packages or product adapters,
  not in Homeboy core.
- WP Codebox artifact schemas stay reusable across WordPress products. A schema
  may describe a browser probe, REST request, WP-CLI command, file artifact, or
  performance observation; it must not encode WooCommerce, Static Site Importer,
  Studio, or other product policy.
- Homeboy Extensions may adapt WordPress runtime output into generic Homeboy
  sidecars and may expose explicit product adapter hooks. Adapter selection is
  caller-owned and opt-in.
- Homeboy core ranks and compares evidence using generic fields such as status,
  severity, timing, resource counters, artifact refs, fingerprints, and opaque
  labels. Core does not interpret product labels beyond displaying, grouping, or
  comparing them.

## Acceptance Criteria

A change preserves this architecture when:

- New product/workload semantics are added to a rig, product repo, or explicit
  product adapter, not to Homeboy core.
- New WP Codebox artifacts are documented as reusable WordPress runtime evidence
  and avoid product policy in schema names, required fields, and validators.
- New Homeboy Extension logic is runtime plumbing or opt-in product adaptation;
  it does not silently infer product semantics from paths, package names, or
  route strings.
- New Homeboy core logic accepts opaque evidence and metadata, with no direct
  dependency on WordPress product names, WP Codebox package internals, or rig
  fixture conventions.
- Reviewer-facing evidence links point to committed docs, issues, PRs, or
  persisted artifacts rather than machine-local paths.
- Tests or fixtures use neutral provider/runtime/product names unless the test is
  explicitly scoped to a product adapter.

## Related Docs

- [Agent task generic loop contract](agent-task-generic-loop-contract.md)
- [Browser evidence schemas](browser-evidence-schemas.md)
- [Provider fanout boundary](provider-fanout-boundary.md)
- [Structured sidecars](structured-sidecars.md)
