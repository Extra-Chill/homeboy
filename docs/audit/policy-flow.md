# Policy-flow detector

The policy-flow detector reports an aggregate transformation that drops declared
policy state before downstream code independently branches over the same typed
decision domain. It consumes resolved, language-neutral facts from fingerprint
extensions; Homeboy core does not parse source syntax or recognize product names.

## Finding conditions

The detector emits `lossy_policy_projection` only when one configured rule has
all of these facts:

- A resolved source aggregate declares every configured `policy_fields` entry.
- A projection maps at least one source field into the configured decision carrier.
- The projection omits at least one configured policy field.
- The configured decision callable has a branch fact with the configured domain type.
- The decision callable has no authoritative call whose result governs that same
  resolved decision domain.

Missing or ambiguous identities produce no finding. Test-path facts are ignored.
Preserving all policy fields or delegating to the authoritative method is clean.
An ordinary DTO projection is clean unless a declaration explicitly connects its
source, carrier, callable, and decision domain.

## Policy declarations

Project/component configuration declares policy semantics under `audit.policy_flow`:

```json
{
  "audit": {
    "policy_flow": {
      "rules": [
        {
          "id": "engagement-policy",
          "source_type_id": "domain::SourcePolicy",
          "policy_fields": ["threshold"],
          "authoritative_method_id": "domain::SourcePolicy::should_engage",
          "decision_sinks": [
            {
              "carrier_type_id": "domain::DecisionCarrier",
              "callable_id": "domain::decide",
              "domain_type_id": "domain::Severity"
            }
          ],
          "convention": "policy_flow",
          "severity": "warning"
        }
      ]
    }
  }
}
```

An extension supplies the same typed declaration under
`audit.detector_rules.policy_flow`. Linked extension and component declarations
merge by rule `id`; the first declaration for an ID wins. Use globally unique IDs.
`convention` defaults to `policy_flow`, and `severity` accepts `warning` or `info`
and defaults to `warning`.

## Fingerprint producer contract

For each source file, the extension fingerprint script may add these arrays to
its existing `FingerprintOutput` JSON object:

```json
{
  "aggregate_definitions": [
    {
      "type_id": "domain::SourcePolicy",
      "fields": [
        {"name": "threshold", "type_id": "domain::Threshold"}
      ],
      "location": {"line": 3, "column": 1}
    }
  ],
  "field_accesses": [
    {
      "owner_type_id": "domain::SourcePolicy",
      "field": "threshold",
      "callable_id": "domain::SourcePolicy::should_engage",
      "access": "read",
      "location": {"line": 5, "column": 9}
    }
  ],
  "aggregate_projections": [
    {
      "source_type_id": "domain::SourcePolicy",
      "target_type_id": "domain::DecisionCarrier",
      "callable_id": "domain::project",
      "field_mappings": [
        {"source_field": "label", "target_field": "label"}
      ],
      "location": {"line": 8, "column": 5}
    }
  ],
  "decision_branches": [
    {
      "callable_id": "domain::decide",
      "domain_type_id": "domain::Severity",
      "discriminant_id": "severity",
      "location": {"line": 21, "column": 5}
    }
  ],
  "method_calls": [
    {
      "caller_id": "domain::decide",
      "target_method_id": "domain::SourcePolicy::should_engage",
      "receiver_type_id": "domain::SourcePolicy",
      "result_used_as_decision": true,
      "decision_domain_type_id": "domain::Severity",
      "location": {"line": 22, "column": 9}
    }
  ]
}
```

All arrays and all `location` fields are optional and default empty. `line` and
`column` are one-based; zero means unavailable. `type_id`, `callable_id`,
`caller_id`, and `target_method_id` must be deterministic canonical identities
qualified enough to distinguish symbols in different modules. `discriminant_id`
is a stable resolved identity chosen by the extension, not raw source text.
`receiver_type_id` and aggregate-field `type_id` are optional when unresolved.
`access` is `read` or `write`. `result_used_as_decision` is true only when the
call result directly governs the caller's decision, such as a returned call or
controlling condition; it defaults to false. A decision-governing call also sets
`decision_domain_type_id`; delegation suppresses only the matching configured
domain.

`field_accesses` is part of the shared fact vocabulary for consumers that need
read/write evidence. This detector treats configured `policy_fields` as the
authoritative semantic declaration rather than requiring the authoritative method
to read each field directly, because that method may delegate internally.

The file path is not repeated in each fact. Homeboy associates facts with the
`file_path` supplied to the fingerprint script. Producers should sort each fact
array and nested `fields`/`field_mappings` deterministically by canonical identity
and source location.

Complete positive and negative serialized examples live in
`tests/fixtures/audit_policy_flow/`.

## Evidence and baselines

The finding is anchored to the projection file and names source, projection, and
decision locations, omitted fields, and the authoritative method. Findings sort
deterministically. Their baseline identity uses the configured convention, rule
ID, canonical source and carrier types, projection callable, decision callable,
decision domain, projection file, and finding kind. Source line movement does not churn
the baseline, while two declared seams in one file remain distinct.

The detector runs through the normal full audit execution plan and supports
`--only lossy_policy_projection`, exclusions, changed-scope reconciliation, and
standard audit baselines.
