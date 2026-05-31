# `homeboy audit`

## Synopsis

```sh
homeboy audit <component-id|path> [options]
```

## Description

Audit a component's codebase for convention drift, structural complexity, dead code, duplication, configured detector drift, and test coverage gaps. The audit engine fingerprints source files, discovers conventions (patterns most files follow), detects outliers, and produces actionable findings.

Works with registered components (by ID) or raw filesystem paths.

## Arguments

- `<component-id|path>`: Component ID to audit, or a direct filesystem path

## Options

- `--conventions`: Only show discovered conventions (skip findings)
- `--extension <ID>`: One-shot extension override for the current invocation; repeat to layer multiple extension hints
- `--only <kind>`: Restrict findings to one or more finding kinds; repeat for multiple kinds
- `--exclude <kind>`: Exclude one or more finding kinds; repeat for multiple kinds
- `--baseline`: Save current audit state as baseline for future comparisons
- `--ignore-baseline`: Skip baseline comparison even if a baseline exists
- `--ratchet`: Auto-update the baseline when the current run improves on it
- `--path <PATH>`: Override `local_path` for this audit run (use a workspace clone or temp checkout)
- `--changed-since <REF>`: Restrict findings to files changed since a git ref
- `--json-summary`: Return compact machine-readable summary (`audit.summary`) for CI wrappers
- `--fixability`: Include automated-fixability metadata by running the refactor planner after audit completes

## Audit Pipeline

The audit runs in 6 phases:

1. **Discovery** — Auto-discover file groups by walking the source tree and fingerprinting files via installed extensions
2. **Convention detection** — For each file group, discover conventions (patterns shared by a majority of files): expected methods, registrations, interfaces, namespaces, imports
3. **Convention checking** — Check all files against discovered conventions, flagging outliers
4. **Findings** — Build actionable findings from multiple analyses:
   - **4a: Convention outliers** — Files missing expected methods/registrations
   - **4b: Structural complexity** — God files, high item counts
   - **4c: Exact duplication** — Identical function bodies across files
   - **4d: Near-duplication** — Structurally similar files with different identifiers
   - **4e: Dead code** — Unused params, unreferenced exports, orphaned internals
   - **4f: Test coverage gaps** — Missing test files, uncovered methods, orphaned tests (requires extension `test_mapping` config)
    - **4h: Layer ownership** — Optional architecture/layer ownership rule violations (`layer_ownership_violation`)
    - **4t: Requested detectors** — Extension/component-owned regex detector packs, including generic config round-trip key asymmetry (`config_roundtrip_asymmetry`)
    - **Remote execution preflight** — Component-owned remote dispatch markers checked for path translation, capability parity, extension parity, artifact snapshot, and artifact retrievability contracts (`remote_execution_preflight`)
5. **Report** — Aggregate findings, compute alignment score
6. **Cross-directory conventions** — Detect patterns shared by sibling subdirectories

## Baseline Workflow

Baselines enable drift detection — track whether code quality is improving or regressing:

```sh
# Save current state as baseline
homeboy audit my-component --baseline

# Future audits compare against baseline automatically
homeboy audit my-component
# Output includes: new findings, resolved findings, drift direction

# Skip baseline comparison for a clean audit
homeboy audit my-component --ignore-baseline
```

The baseline is saved in `homeboy.json` under `baselines.audit` inside the component's `local_path`.

Layer ownership rule config is optional and read from either:

- `homeboy.json` key: `audit_rules`

See: `docs/commands/audit-rules.md`

When a baseline exists, the audit exit code reflects drift:
- `0`: No drift increase (same or improved)
- `1`: Drift increased (new findings since baseline)

## Fixing Findings

The audit command is read-only — it finds problems but does not fix them. To apply automated fixes, use the refactor command:

```sh
# Preview audit fixes (dry run)
homeboy refactor my-component --from audit

# Apply audit fixes to disk
homeboy refactor my-component --from audit --write

# Fix all sources (audit + lint + test) in one pass
homeboy refactor my-component --from all --write
```

## Examples

```sh
# Audit a registered component
homeboy audit data-machine

# Audit a raw filesystem path
homeboy audit /path/to/project

# Show only conventions (no findings)
homeboy audit homeboy --conventions

# Drill into or suppress specific finding kinds
homeboy audit homeboy --only missing_test_file
homeboy audit homeboy --only remote_execution_preflight
homeboy audit homeboy --exclude near_duplicate_file

# Save baseline after a cleanup sprint
homeboy audit my-plugin --baseline

# Let a cleanup sprint ratchet an improved baseline forward
homeboy audit my-plugin --ratchet

# Audit a workspace clone instead of local_path
homeboy audit my-plugin --path /var/lib/datamachine/workspace/my-plugin

# Force a one-shot extension when the checkout has no registered component
homeboy audit --path /path/to/worktree --extension rust
```

## JSON Output

`--json-summary` keeps large audit runs compact by returning grouped finding buckets in
`finding_groups`. Each bucket includes count/severity totals, a few sample files, and a
ready-to-run drill-down command scoped to that finding kind.

```json
{
  "command": "audit.summary",
  "total_findings": 87,
  "warnings": 80,
  "info": 7,
  "finding_groups": [
    {
      "kind": "missing_test_file",
      "count": 34,
      "warnings": 34,
      "info": 0,
      "confidence": "heuristic",
      "sample_files": ["src/commands/audit.rs", "src/core/code_audit/report.rs"],
      "drilldown_command": "homeboy audit homeboy --only missing_test_file"
    }
  ],
  "exit_code": 1
}
```

```json
{
  "success": true,
  "data": {
    "command": "audit",
    "component_id": "my-plugin",
    "source_path": "/path/to/source",
    "summary": {
      "files_scanned": 45,
      "conventions_detected": 3,
      "outliers_found": 5,
      "alignment_score": 0.89,
      "files_skipped": 2,
      "warnings": []
    },
    "conventions": [
      {
        "name": "Steps",
        "glob": "inc/Steps/*.php",
        "status": "partial",
        "expected_methods": ["register", "validate", "execute"],
        "conforming": ["step_a.php", "step_b.php"],
        "outliers": [
          {
            "file": "step_c.php",
            "deviations": [
              { "kind": "missing_method", "detail": "validate" }
            ]
          }
        ],
        "total_files": 3,
        "confidence": 0.85
      }
    ],
    "findings": [
      {
        "file": "inc/Steps/step_c.php",
        "severity": "warning",
        "category": "convention_outlier",
        "description": "Missing method 'validate' expected by Steps convention"
      }
    ],
    "duplicate_groups": [],
    "directory_conventions": []
  }
}
```

With `--baseline` comparison:

```json
{
  "success": true,
  "data": {
    "command": "audit.compared",
    "component_id": "my-plugin",
    "summary": { "..." : "..." },
    "baseline_comparison": {
      "drift_increased": false,
      "new_findings": [],
      "resolved_findings": ["inc/Steps/step_c.php: missing validate"],
      "baseline_findings_count": 6,
      "current_findings_count": 5
    }
  }
}
```

## Exit Code

- `0`: No outliers found (or no drift increase when baseline exists)
- `1`: Outliers found (or drift increased since baseline)

## Related

- [docs](docs.md) — embedded documentation topics and codebase maps
- [lint](lint.md) — extension-driven code style validation
- [JSON output contract](../architecture/output-system.md)
