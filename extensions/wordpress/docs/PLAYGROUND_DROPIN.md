# Playground backend: db.php drop-in coexistence

The Playground test backend supports plugins that ship their own `db.php`
drop-in (WordPress's mechanism for swapping out `$wpdb`). This document
explains the mechanism, shows how to verify it end-to-end, and documents the
upstream Playground behavior this integration relies on.

## Summary

**Plugins with a `db.php` drop-in do not need any special configuration.**
The Playground backend detects `<plugin>/db.php` and mounts it to
`/wordpress/wp-content/db.php` inside the VFS. Playground's built-in SQLite
integration then voluntarily steps aside and lets the drop-in own `$wpdb`.

Concretely:

```
plugin/
├── main-plugin.php     # Plugin entry
├── db.php              # Drop-in — gets mounted to /wp-content/db.php
└── tests/
    └── SomeTest.php    # Runs with plugin's db.php in charge of $wpdb
```

The runner handles the mount. You just place `db.php` in the plugin root.

## How coexistence works

WordPress Playground normally ships an mu-plugin at
`/internal/shared/mu-plugins/sqlite-database-integration.php` that wires up
`$wpdb` to its bundled SQLite implementation. The first few lines of that
mu-plugin are a guard:

```php
// Do not preload this if WordPress comes with a custom db.php file.
if ( file_exists( '/wordpress/wp-content/db.php' ) ) {
    return;
}
```

So the load order during a Playground request is:

1. **WordPress core** (`wp-settings.php`) calls `require_wp_db()`, which
   checks for `/wordpress/wp-content/db.php` and, if present, `require_once`s
   it. The drop-in sets `$wpdb` and defines any constants it needs.
2. **Playground's SQLite mu-plugin** loads, sees the drop-in exists, and
   returns immediately. `$wpdb` stays as whatever the drop-in set it to.
3. **WordPress** finishes booting with the drop-in's `$wpdb` in charge.

No `--skip-sqlite-setup` flag is needed. The runner does not pass it and
you should not either — if you pass `--skip-sqlite-setup` AND mount a
drop-in that relies on Playground's bundled SQLite classes, the
`/internal/shared/sqlite-database-integration/` directory is still available
on the VFS (mu-plugins are config, the library is filesystem), so the drop-in
still works. But the full WordPress install path tries to initialize MySQL
and fails early. Skip-sqlite-setup is for a different use case (host-MySQL
testing), not for drop-in coexistence.

## The drop-in can reuse Playground's bundled SQLite

This is the killer feature. Playground's SQLite implementation lives at
`/internal/shared/sqlite-database-integration/` in the VFS and is readable
from the drop-in:

```php
<?php
// Your plugin's db.php

$sqlite = '/internal/shared/sqlite-database-integration';
if ( ! file_exists( $sqlite . '/wp-includes/sqlite/db.php' ) ) {
    return; // fall back to MySQL
}

if ( ! defined( 'DB_ENGINE' ) ) {
    define( 'DB_ENGINE', 'sqlite' );
}

// Delegate to Playground's implementation — this sets $wpdb = WP_SQLite_DB.
require_once $sqlite . '/wp-includes/sqlite/db.php';

// ...then wrap $wpdb, subclass it, or replace it entirely with your own.
```

This is the pattern [markdown-database-integration](https://github.com/Automattic/markdown-database-integration)
uses in production: delegate to Playground's bundled SQLite for the query
engine, then substitute a custom `$wpdb` subclass that adds markdown
mirroring on top.

## Verifying with the fixture

A reference fixture lives at `wordpress/tests/fixtures/dropin-coexistence/`:

```
tests/fixtures/dropin-coexistence/
├── plugin.php                          # Inert plugin entry
├── db.php                              # Reference drop-in (delegates to bundled SQLite)
└── tests/
    └── DropInCoexistenceTest.php      # Asserts coexistence end-to-end
```

The WP Codebox test runner runs this fixture and checks all three invariants:

```bash
HOMEBOY_COMPONENT_ID=dropin-coexistence \
HOMEBOY_COMPONENT_PATH=wordpress/tests/fixtures/dropin-coexistence \
HOMEBOY_EXTENSION_PATH=wordpress \
bash wordpress/scripts/test/test-runner.sh
```

Expected output:

```
OK (3 tests, 8 assertions)
ALL TESTS PASSED
```

The three assertions cover:

1. **Drop-in actually loaded.** `HOMEBOY_DROPIN_FIXTURE_LOADED` is defined
   only by `db.php`. If undefined, WordPress never `require_once`d the
   drop-in — check the mount.
2. **`$wpdb` works end-to-end.** `wp_insert_post` + `get_post` round-trip
   real data through the drop-in's `$wpdb`.
3. **Playground's mu-plugin stepped aside.** `SQLITE_DB_DROPIN_VERSION` is
   defined at the top of Playground's mu-plugin body. If it IS defined, the
   mu-plugin's guard failed and the drop-in was silently overwritten.

Run the smoke test after:

- Changes to `scripts/test/test-runner-wp-codebox.sh` (the WP Codebox dispatcher)
- Changes to WP Codebox's WordPress runtime commands
- Upgrading WP Codebox's Playground runtime (upstream may change the mu-plugin guard)

## Upstream contract

This whole mechanism depends on the guard at the top of
`/internal/shared/mu-plugins/sqlite-database-integration.php`. If a future
Playground release removes or changes that guard, the coexistence model
breaks. The `test_playground_mu_plugin_stepped_aside` assertion in the
fixture catches that regression.

Source of the guard:
[WordPress/wordpress-playground — packages/playground/wordpress-builds/...](https://github.com/WordPress/wordpress-playground)
(look for the mu-plugin that's auto-generated from the SQLite integration
during build).

## Limitations

- **WordPress install flow.** Playground's default install flow assumes
  SQLite is available. If your drop-in cannot serve the install flow (e.g.,
  it needs external state to be set up first), the tests will die with
  "Error connecting to the database" before the test runner starts. Fix:
  make sure the drop-in can serve a cold-boot install, which typically
  means it must delegate to Playground's bundled SQLite for the initial
  schema.

- **WP version pinning.** The WordPress test runner defaults to `--wp=6.9`, and
  the `playground_wordpress_version` setting can pass a different `--wp=<version>`.
  The selected WordPress version must
  match the `wp-phpunit` package version. Don't forget to update both when
  bumping WordPress. A mismatch often manifests as missing `WP_UnitTestCase`
  factory methods, not as a database error — the drop-in will load fine but
  wp-phpunit bootstrap will fail.

- **MDI-specific note.** Markdown Database Integration's production drop-in
  assumes its own plugin directory is at
  `wp-content/plugins/markdown-database-integration/` (to load its classes).
  When testing MDI through the WordPress test backend, the MDI plugin itself is
  mounted to that location by the runner. You don't have
  to configure anything special for this common case.

## Related

- Issue [#214](https://github.com/Extra-Chill/homeboy-extensions/issues/214)
  (Playground backend umbrella)
- PR [#215](https://github.com/Extra-Chill/homeboy-extensions/pull/215)
  (Phase 1: initial Playground backend)
- PR [#216](https://github.com/Extra-Chill/homeboy-extensions/pull/216)
  (Phase 2: structured diagnostics)
- PR [#217](https://github.com/Extra-Chill/homeboy-extensions/pull/217)
  (Phase 2: phpunit.xml.dist-aware recursive discovery)
