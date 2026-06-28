# Compiler Warning Extension Contract Gap

Issue #2242 asks Homeboy core to move compiler-warning collection and compiler-warning fix generation behind extension-owned contracts. Validation and formatting already have extension script contracts (`scripts.validate` and `scripts.format`), but compiler warnings do not.

## Required Upstream Contract

Homeboy core needs an extension manifest contract for two separate capabilities before `src/core/code_audit/compiler_warnings.rs` and `src/core/refactor/plan/generate/compiler_warning_fixes.rs` can be made language-agnostic without dropping behavior.

1. `scripts.compiler_warnings`
   - Runs from the component root.
   - Receives JSON on stdin with at least `{ "root": string }`.
   - Emits a generic warnings envelope on stdout.
   - Core maps the envelope to `AuditFinding::CompilerWarning` findings.

2. `scripts.compiler_warning_fixes`
   - Runs from the component root after compiler warning findings exist.
   - Receives JSON on stdin with `{ "root": string, "findings": [...] }`.
   - Emits a generic fix-suggestion envelope on stdout.
   - Core maps line removals and line replacements to existing refactor primitives.

## Proposed Warning Envelope

```json
{
  "warnings": [
    {
      "code": "unused_imports",
      "message": "unused import",
      "file": "src/lib.rs",
      "line": 3,
      "suggestion": "Remove the unused import"
    }
  ]
}
```

## Proposed Fix Envelope

```json
{
  "fixes": [
    {
      "file": "src/lib.rs",
      "kind": "line_replacement",
      "line_start": 6,
      "line_end": 6,
      "original_text": "mut ",
      "replacement": "",
      "message": "Remove unused mut"
    }
  ]
}
```

Allowed `kind` values should start with `line_replacement` and `line_removal`, because those map cleanly to Homeboy's existing generic edit primitives. Ecosystem-specific parsing of `cargo check --message-format=json` should move into the Rust extension once this contract exists.

## Blocker

Do not replace the current Cargo compiler-warning implementation with another core fallback. The next upstream PR should add manifest fields, parser structs, extension invocation, and tests with a fake extension script before the Cargo implementation is removed from core.
