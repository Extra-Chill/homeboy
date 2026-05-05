# WordPress Audit Core Contract Gaps

The WordPress extension owns PHP and WordPress semantics. Homeboy core should stay language-agnostic and expose generic audit contracts that extensions can configure.

## Supported Today

The extension now uses the generic knobs Homeboy already exposes:

- `audit.detector_rules.lifecycle_path_globs` for non-production guard contexts such as tests, smokes, shims, stubs, and fallback files.
- `audit.detector_rules.utility_suffixes` for PHP role suffixes that should not be forced into a sibling class convention.
- `audit.detector_rules.convention_exception_globs` for procedural helper files such as `register-*.php` and `*-functions.php`.
- A narrower `wordpress-constant-backed-slug-literal` pattern that only reports comparison contexts, not array keys or event/protocol names.

## Missing Generic Core Contract

Some WordPress/PHP false-positive shapes need generic core hooks before the extension can express them fully.

### Comment-Based Dead-Guard Contexts

Core `dead_guard` can exempt paths and known lifecycle callbacks, but not source comments near a guard. The extension needs a generic comment-context exemption, for example:

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

### Role-Aware Convention Families

Core can mark suffixes as utility-like after a directory convention is inferred, but it cannot yet split convention inference by extension-owned file roles before method/signature comparison. The extension needs a generic role classifier, for example:

```json
{
  "audit": {
    "detector_rules": {
      "file_roles": [
        { "role": "procedural_helper", "path_globs": ["**/register-*.php", "**/*-functions.php"] },
        { "role": "contract", "type_suffixes": ["Interface", "Contract", "Store"] },
        { "role": "lock", "type_suffixes": ["Lock"] },
        { "role": "value", "type_suffixes": ["Value", "Result", "Payload", "Package"] }
      ],
      "convention_group_key": ["directory", "role"]
    }
  }
}
```

Expected core behavior: convention discovery, missing-method checks, naming checks, and signature consistency compare files only within compatible role families. That keeps procedural helpers out of class conventions, store contracts out of lock implementations, and package result/value objects out of one constructor-signature convention.

### Requested Detector Context Filters

Core requested detectors currently support language, extension, and path filters. If the slug-literal detector needs to recover broader matching later, it needs generic match-context filters, for example:

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
