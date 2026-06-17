# WordPress Audit Core Contract Gaps

The WordPress extension owns PHP and WordPress semantics. Homeboy core should stay language-agnostic and expose generic audit contracts that extensions can configure.

## Supported Today

The extension uses these generic knobs Homeboy already exposes:

- `audit.detector_rules.lifecycle_path_globs` for non-production guard contexts such as tests, smokes, shims, stubs, and fallback files.
- `audit.detector_rules.utility_suffixes` for PHP role suffixes that should not be forced into a sibling class convention when a dominant naming suffix exists.
- `audit.detector_rules.convention_exception_globs` for files that should be fully exempt from missing-method/registration/interface checks, such as procedural helpers (`register-*.php`, `*-functions.php`) and PHP fixer scripts.
- A narrower `wordpress-constant-backed-slug-literal` pattern that only reports comparison contexts, not array keys or event/protocol names.

## Role-Aware Convention Grouping (`convention_tag_globs`)

Homeboy core supports `audit.detector_rules.convention_tag_globs` on `main`. This is the contract the WordPress extension uses to separate unrelated PHP roles within the same directory. Each rule attaches an opaque tag to file globs:

```json
{
  "audit": {
    "detector_rules": {
      "convention_tag_globs": [
        {
          "tag": "wordpress:php-role:contract",
          "globs": [
            "**/class-*-store.php",
            "**/class-*-adopter.php",
            "**/class-*-resolver.php"
          ]
        },
        {
          "tag": "wordpress:php-role:registry",
          "globs": ["**/class-*-registry.php"]
        }
      ]
    }
  }
}
```

Core does not interpret the tag string. It uses tag membership as part of the convention group key, so files with different tags inside the same directory land in different convention groups. That keeps store contracts, registries, adopter interfaces, service/authenticator classes, credential/token objects, policy/configuration vocabularies, result/diff objects, factories, and value objects out of one shared method/signature convention.

Extensions own the role taxonomy. Tags are namespaced (`wordpress:php-role:*`) so multiple extensions in the same component never collide.

## Known-Symbol Curation for Dual-Context Code

Core `dead_guard` flags any `function_exists`/`class_exists`/`defined` guard whose symbol the extension declared in `audit.detector_rules.known_symbols`. The extension owns that list. The contract is "guaranteed to exist at runtime", which is stronger than "ships with WordPress".

Some WordPress library code legitimately runs in two contexts:

- Inside a loaded WordPress install, where the WP function is available.
- Inside a pure-PHP smoke test or vendored consumer, where it is not.

For these dual-context shapes the conventional WordPress idiom is a defensive guard with a pure-PHP fallback:

```php
private static function json_encode( $data ) {
    if ( function_exists( 'wp_json_encode' ) ) {
        return wp_json_encode( $data );
    }

    // phpcs:ignore WordPress.WP.AlternativeFunctions.json_encode_json_encode -- Pure-PHP smoke tests run without WordPress loaded.
    return json_encode( $data );
}
```

This is correct production code, not a dead branch. The extension uses `dead_guard_context_comment_patterns` for those call-site comments and curates `known_symbols.header_versions` along a single principle:

- **Include** WordPress symbols that have no natural pure-PHP equivalent and whose presence implies "WordPress is loaded": REST classes (`WP_REST_Server`, `WP_REST_Request`, `WP_REST_Response`), block infrastructure (`WP_Block`, `WP_Block_Type_Registry`, `register_block_type`, `parse_blocks`, `has_blocks`), HTML processing (`WP_HTML_Tag_Processor`), abilities (`WP_Ability`), REST routing (`register_rest_route`), post-type metadata (`get_post_type_object`), environment classification (`wp_get_environment_type`), and the `REST_REQUEST` constant.
- **Include** WordPress utility wrappers that are interchangeable with a pure-PHP standard-library call, but only exempt dual-context guard sites whose nearby comments identify the pure-PHP fallback path: JSON encoding (`wp_json_encode` ↔ `json_encode`), UUID generation (`wp_generate_uuid4` ↔ `random_bytes` + `bin2hex`), URL parsing (`wp_parse_url` ↔ `parse_url`), date/timezone helpers (`wp_date`, `wp_timezone`, `wp_timezone_string` ↔ `DateTimeImmutable`/`DateTimeZone`).

Comment-marked fallback guards are treated as contextual, while the same utility wrappers still produce real `dead_guard` findings when guarded in unambiguously WordPress-only code. WP-only symbols also continue to produce findings when guarded outside lifecycle/test contexts. These behaviors are exercised by `tests/audit-detector-config-smoke.sh` against fixtures under `tests/fixtures/audit-dead-guard-fallback/` and `tests/fixtures/audit-dead-guard-wp-only/`.

This curation is generic: it expresses "what WordPress utility functions have natural pure-PHP fallbacks" without naming any consumer plugin's paths.

## Other Generic Core Hooks Still Missing

### Comment-Based Dead-Guard Contexts

Status: implemented in the WordPress extension config via `dead_guard_context_comment_patterns`.

Core `dead_guard` can exempt paths and known lifecycle callbacks, but not source comments near a guard. A more granular comment-context exemption would let the extension keep utility wrappers in `known_symbols` while letting individual call sites mark themselves as dual-context. Sketch:

```json
{
  "audit": {
    "detector_rules": {
      "dead_guard_context_comment_patterns": [
        "(?i)pure-php smoke tests run without wordpress loaded",
        "(?i)fallback when wordpress is not loaded"
      ]
    }
  }
}
```

Expected core behavior: when a guard is on or near a matching comment block, treat it as contextual and do not emit `dead_guard` even if the guarded symbol is known in production runtime metadata.

### Requested Detector Context Filters

Status: implemented in the WordPress extension config via `exclude_match_context_patterns`.

The slug-literal detector keeps its literal regex broad and uses generic match-context filters for contexts where WordPress conventions make a repeated literal acceptable:

```json
{
  "id": "wordpress-constant-backed-slug-literal",
  "exclude_match_context_patterns": [
    "['\"]{value}['\"]\\s*=>",
    "do_action\\s*\\(\\s*['\"]{value}['\"]",
    "apply_filters\\s*\\(\\s*['\"]{value}['\"]"
  ]
}
```

Expected core behavior: collect candidate matches normally, then drop matches whose surrounding source span satisfies an extension-owned context-exclusion regex.
