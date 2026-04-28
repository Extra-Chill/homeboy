# Homeboy WordPress Extension

Universal test and lint infrastructure for WordPress plugins and themes. Zero configuration required.

## Features

- **PHPUnit** with full WordPress bootstrap (SQLite or MySQL)
- **PHPCS** with WordPress coding standards
- **ESLint** with WordPress JavaScript standards
- **Automatic text domain detection** from plugin/theme headers
- **Test discovery** - just add test files to `tests/`

## Quick Start

```bash
# Test a plugin
homeboy test extrachill-users

# Lint a plugin
homeboy lint extrachill-blog

# Fix lint issues
homeboy lint extrachill-blog --fix

# Build for production
homeboy build extrachill-shop
```

## How It Works

### Test Discovery

The extension automatically discovers tests when a `tests/` directory exists with PHP files:

```
your-plugin/
├── your-plugin.php
├── inc/
│   └── ...
└── tests/
    ├── test-feature-one.php    ← Discovered automatically
    └── test-feature-two.php    ← Discovered automatically
```

No `bootstrap.php` or `phpunit.xml` needed in your plugin - the extension provides everything.

### Linting

**PHP (PHPCS)**:
- WordPress Coding Standards
- Auto-detects text domain from `Text Domain:` header
- Advisory: warns on issues, continues to tests

**JavaScript (ESLint)**:
- WordPress JavaScript Standards
- Auto-detects text domain from plugin header
- Skips if no JS/JSX/TS/TSX files found
- Advisory: warns on issues, continues to tests

### Build Process

```
homeboy build <component>
    │
    ├─ Run tests (while dev deps available)
    │   ├─ PHPCS linting → Advisory
    │   ├─ ESLint linting → Advisory
    │   └─ PHPUnit tests → Blocks on failure
    │
    ├─ Install production dependencies
    ├─ Build frontend assets (if package.json exists)
    ├─ Copy files (respects .buildignore)
    ├─ PHP syntax validation → Blocks on failure
    └─ Create ZIP, restore dev deps
```

## Writing Tests

Create test files in `tests/` that extend `WP_UnitTestCase`:

```php
<?php
// tests/test-my-feature.php

class Test_My_Feature extends WP_UnitTestCase {

    public function test_something() {
        $this->assertTrue( true );
    }

    public function test_user_creation() {
        $user_id = $this->factory->user->create();
        $this->assertIsInt( $user_id );
    }

    public function test_post_creation() {
        $post_id = $this->factory->post->create([
            'post_title' => 'Test Post',
            'post_status' => 'publish',
        ]);
        $this->assertEquals( 'Test Post', get_the_title( $post_id ) );
    }
}
```

### Available Test Factories

The WordPress test framework provides factories for creating test data:

- `$this->factory->user` - Create users
- `$this->factory->post` - Create posts
- `$this->factory->comment` - Create comments
- `$this->factory->term` - Create taxonomy terms
- `$this->factory->category` - Create categories
- `$this->factory->tag` - Create tags
- `$this->factory->attachment` - Create attachments

## Configuration

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `HOMEBOY_SKIP_LINT` | Skip PHPCS and ESLint | `0` |
| `HOMEBOY_SKIP_TESTS` | Skip PHPUnit tests | `0` |
| `HOMEBOY_DEBUG` | Show debug output | `0` |

### Test Backend

The extension supports two test execution backends:

| Backend | Description | Default |
|---------|-------------|---------|
| `playground` | PHPUnit inside WordPress Playground (PHP-WASM + SQLite) | Yes |
| `host-smoke` / `host` | Standalone `tests/**/*-smoke.php` scripts under host PHP | No |

```bash
# Switch a component to Playground backend
homeboy component set my-plugin test_backend playground

# Run standalone host PHP smoke scripts instead
homeboy component set my-plugin test_backend host-smoke
```

The Playground backend is the default. It boots a WordPress Playground
instance, mounts the plugin, and runs PHPUnit inside PHP-WASM. No host PHP or
MySQL is required.

The host-smoke backend is for pure PHP smoke suites that do not need WordPress.
It discovers `tests/**/*-smoke.php`, runs each file in its own host PHP process,
and fails fast with the failing script name.

**Limitations (Phase 1):**
- WordPress version is pinned to match the wp-phpunit package (currently 6.9.x).
- Custom `db.php` drop-ins may conflict with Playground's built-in SQLite integration.

### Database Options

The extension supports MySQL or SQLite for tests. The default is **`auto`**: the
test runner tries to connect to MySQL first (using explicit settings, then a
`wp-config.php` discovered in the component's parent tree, then `root@127.0.0.1`
with no password) and falls back to SQLite if none of those succeed.

```bash
# Auto (default): MySQL if reachable, otherwise SQLite
homeboy test my-plugin

# Force SQLite — useful on machines without a MySQL server
homeboy test my-plugin --setting database_type=sqlite

# Force MySQL — fails loudly if MySQL isn't reachable
homeboy test my-plugin --setting database_type=mysql
```

Configure MySQL credentials with `homeboy component set <id> mysql_host ...`
(also `mysql_user`, `mysql_password`, `mysql_database`). To make SQLite the
default for a specific component, set it once:

```bash
homeboy component set my-plugin database_type sqlite
```

## Migration from Local Infrastructure

If your plugin has local test infrastructure, the extension will warn and ignore it:

```
⚠ Warning: Local bootstrap.php found and will be IGNORED
  Location: /path/to/plugin/tests/bootstrap.php
  Homeboy WordPress extension provides complete test infrastructure.
  Consider removing: /path/to/plugin/tests/bootstrap.php
```

Files that can be safely removed after migration:
- `tests/bootstrap.php`
- `phpunit.xml` or `phpunit.xml.dist`
- Local PHPCS/ESLint configs (if using extension standards)

## Extension Structure

```
wordpress/
├── scripts/
│   ├── test/
│   │   ├── test-runner.sh              # Main test router
│   │   ├── test-runner-host-smoke.sh   # Host PHP smoke-script backend
│   │   ├── test-runner-playground.sh   # Playground backend runner
│   │   ├── generate-config.sh          # WordPress config generation
│   │   ├── parse-test-results.sh       # Result parsing for homeboy core
│   │   └── parse-test-failures.sh      # Failure parsing for homeboy core
│   ├── build.sh            # Production build script
│   ├── lint.sh             # Standalone linting
│   └── generate-config.sh  # WordPress config generation
├── tests/
│   └── bootstrap.php       # Universal WordPress bootstrap
├── phpunit.xml.dist        # PHPUnit configuration
├── phpcs.xml.dist          # PHPCS configuration
├── .eslintrc.json          # ESLint configuration
├── composer.json           # PHP dependencies
└── package.json            # Node dependencies (ESLint + @wp-playground/cli)
```

## Blocking vs Advisory

| Check | Behavior |
|-------|----------|
| PHPCS (PHP linting) | Advisory - warns, continues |
| ESLint (JS linting) | Advisory - warns, continues |
| PHPUnit (tests) | **Blocks** - fails build on error |
| PHP syntax (`php -l`) | **Blocks** - fails build on error |

## Requirements

- PHP 8.1+
- Node.js 18+ (for ESLint)
- Composer (for PHP dependencies)
