# Repeated literal-shape detector

Detects inline associative array literals that appear many times with the same
shape (ordered keys + value kinds) across a component. Shape-level repetition
is a strong signal that the call sites should share a helper constructor —
e.g. the ubiquitous `['success' => false, 'error' => $x, 'message' => $y]`
envelope should become `error_envelope($error, $message)`.

## What counts as a shape

A shape is the ordered tuple of `(key, value_kind)` pairs, where `value_kind`
is one of: `bool`, `int`, `string`, `null`, `var`, `expr`. Concrete values are
discarded — only structure matters. PHP's array order is preserved (keys are
not sorted).

Two literals with the same keys in a different order are distinct shapes; two
literals with the same keys and compatible value kinds in the same order
collapse to one shape.

## Scope

- **Language:** PHP only for the first pass.
- **Syntax:** `[...]` short arrays and `array(...)` long arrays.
- **Positional/list-only arrays** (no `=>` arrows) are intentionally skipped —
  they are rarely interesting for helper extraction.
- **Nested literals** inside a value are treated as a single `expr` token; the
  detector does not recurse on the first pass.

## Threshold

A finding is emitted when a shape occurs **at least 20 times** across the
component. The finding description lists:

- The shape signature (keys + value kinds).
- Total occurrence count.
- Top three files by occurrence count.
- An estimated LOC reduction if the shape is extracted into a helper.

## Severity

`Info` — the detector's output is plan-only. A separate fixer (planned) will
propose a helper name derived from the key set and a list of call sites to
rewrite. No rewrites are applied automatically.

## Related

- `repeated_field_pattern` — the same idea applied to struct/class field
  declarations instead of inline literals.
