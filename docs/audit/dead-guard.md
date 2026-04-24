# Dead Guard Detection

Homeboy audit flags `function_exists('…')`, `class_exists('…')`, and `defined('…')` guards whose
checked symbol is guaranteed to exist at runtime. Such guards are reachable-but-dead: the `else`
branch can never fire, the code becomes harder to read, and refactors keep carrying forward stale
defensive scaffolding.

## How the detector decides a symbol is guaranteed

A symbol is considered guaranteed when any of the following hold:

- The plugin declares `Requires at least: X.Y` in its main-file header and the symbol shipped in
  Core at or before that version (table covers common symbols such as `WP_Ability` @ 6.9,
  `wp_generate_uuid4` @ 4.7, `wp_timezone` @ 5.3).
- The plugin's main file contains an **unconditional** `require` / `require_once` of a well-known
  vendor bootstrap (e.g. `vendor/woocommerce/action-scheduler/action-scheduler.php`). Requires
  inside an `if ( ! class_exists(…) ) { … }` block are ignored.
- `composer.json` lists a known package under `require` or `require-dev` whose symbols the detector
  recognizes (e.g. `woocommerce/action-scheduler`).

Both direct and negated guards are reported:

```php
if ( ! class_exists( 'WP_Ability' ) ) { return; } // flagged
if ( function_exists( 'as_schedule_single_action' ) ) { … }  // flagged when AS is bootstrapped
```

## Finding output

- `convention`: `dead_guard`
- `kind`: `dead_guard`
- `severity`: `warning`

Dead-guard findings participate in baseline comparisons like any other audit finding.

## Extending the symbol table

The WP-core symbol table lives in `src/core/code_audit/requirements.rs`. Add new rows as
`(symbol_name, introduced_in_encoded_version, kind)` — `kind` is `'f'` for functions, `'c'` for
classes, `'k'` for constants. Version is encoded as `major * 100 + minor` (e.g. 6.9 → 609).

Vendor packages are seeded via `seed_vendor_symbols_from_path` (bootstrap-require match) and
`apply_composer_requires` (composer.json match). Add new packages there.
