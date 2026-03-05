# `homeboy audit`

## Synopsis

```sh
homeboy audit <component-id|path> [options]
```

## Description

Audit a component's codebase for convention drift, structural complexity, dead code, duplication, and test coverage gaps. The audit engine fingerprints source files, discovers conventions (patterns most files follow), detects outliers, and produces actionable findings.

Works with registered components (by ID) or raw filesystem paths.

## Arguments

- `<component-id|path>`: Component ID to audit, or a direct filesystem path

## Options

- `--conventions`: Only show discovered conventions (skip findings)
- `--fix`: Generate fix stubs for outlier files (dry run by default)
- `--write`: Apply fixes to disk (requires `--fix`)
- `--baseline`: Save current audit state as baseline for future comparisons
- `--ignore-baseline`: Skip baseline comparison even if a baseline exists
- `--path <PATH>`: Override `local_path` for this audit run (use a workspace clone or temp checkout)
- `--changed-since <REF>`: Restrict findings to files changed since a git ref
- `--json-summary`: Return compact machine-readable summary (`audit.summary`) for CI wrappers

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

When a baseline exists, the audit exit code reflects drift:
- `0`: No drift increase (same or improved)
- `1`: Drift increased (new findings since baseline)

## Fix Stubs

The `--fix` flag generates mechanical fixes for convention outliers:

```sh
# Preview fixes (dry run)
homeboy audit my-component --fix

# Apply fixes to disk
homeboy audit my-component --fix --write
```

Fix stubs insert missing method declarations, registration calls, and interface implementations that the convention expects.

## Examples

```sh
# Audit a registered component
homeboy audit data-machine

# Audit a raw filesystem path
homeboy audit /path/to/project

# Show only conventions (no findings)
homeboy audit homeboy --conventions

# Save baseline after a cleanup sprint
homeboy audit my-plugin --baseline

# Audit a workspace clone instead of local_path
homeboy audit my-plugin --path /var/lib/datamachine/workspace/my-plugin
```

## JSON Output

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

- [cleanup](cleanup.md) — config-level health checks (complementary to code audit)
- [docs audit](docs.md) — documentation-level auditing (broken links, stale references)
- [lint](lint.md) — extension-driven code style validation
- [JSON output contract](../architecture/output-system.md)
