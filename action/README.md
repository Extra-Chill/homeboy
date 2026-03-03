# Homeboy Action

GitHub Action for running [Homeboy](https://github.com/Extra-Chill/homeboy) lint, test, and audit commands on your PRs.

Works with **any Homeboy extension** — WordPress, Rust, Node, or your own custom extension.

## Quick Start

### WordPress Plugin/Theme

```yaml
name: Homeboy
on: [pull_request]

jobs:
  homeboy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: Extra-Chill/homeboy-action@v1
        with:
          extension: wordpress
          commands: lint,test
          php-version: '8.2'
```

### With Portable Config (`homeboy.json`)

If your repo has a `homeboy.json` file, you don't even need to specify the extension:

```json
{
  "extensions": {
    "wordpress": {}
  }
}
```

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    commands: lint,test
    php-version: '8.2'
```

### Custom Extension

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: my-extension
    extension-source: https://github.com/my-org/my-homeboy-extensions
    commands: lint,test
```

## Inputs

| Input | Required | Default | Description |
|-------|----------|---------|-------------|
| `version` | No | `latest` | Homeboy version to install (e.g. `0.52.0`) |
| `extension` | No | | Extension ID (e.g. `wordpress`, `rust`, `node`) |
| `extension-source` | No | `Extra-Chill/homeboy-extensions` | Git URL to install the extension from |
| `commands` | No | `lint,test` | Comma-separated commands to run |
| `component` | No | *(repo name)* | Component name (auto-detected from repo) |
| `args` | No | | Extra arguments passed to each command |
| `settings` | No | | JSON settings for the extension |
| `php-version` | No | | PHP version (sets up via `shivammathur/setup-php`) |
| `node-version` | No | | Node.js version (sets up via `actions/setup-node`) |

## Outputs

| Output | Description |
|--------|-------------|
| `results` | JSON object with pass/fail for each command (e.g. `{"lint":"pass","test":"fail"}`) |

## Examples

### Lint Only (Fast PR Check)

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint
    args: --errors-only
    php-version: '8.2'
```

### Full Suite with Audit

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint,test,audit
    php-version: '8.2'
    node-version: '20'
```

### Test with Custom Settings

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: test
    settings: '{"database_type": "sqlite"}'
    php-version: '8.2'
```

### Skip Lint During Test (Run Separately)

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: lint
    php-version: '8.2'

- uses: Extra-Chill/homeboy-action@v1
  with:
    extension: wordpress
    commands: test
    args: --skip-lint
    php-version: '8.2'
```

### Pin a Specific Homeboy Version

```yaml
- uses: Extra-Chill/homeboy-action@v1
  with:
    version: '0.52.0'
    extension: wordpress
    commands: lint,test
    php-version: '8.2'
```

### Use Results in Subsequent Steps

```yaml
- uses: Extra-Chill/homeboy-action@v1
  id: homeboy
  continue-on-error: true
  with:
    extension: wordpress
    commands: lint,test
    php-version: '8.2'

- name: Check results
  run: |
    echo "Results: ${{ steps.homeboy.outputs.results }}"
```

## How It Works

1. **Installs Homeboy** — Downloads the correct binary for your runner from GitHub Releases
2. **Installs Extension** — Clones and sets up the specified extension (runs `composer install`, etc.)
3. **Registers Component** — Creates a component config pointing at your checkout (or uses `homeboy.json`)
4. **Runs Commands** — Executes each command with `--path` pointing at your workspace

The action is **extension-agnostic** — Homeboy is the orchestrator, extensions provide the actual lint/test/audit logic. The WordPress extension runs PHPCS, PHPUnit, and PHPStan. Other extensions can run whatever tools they need.

## Requirements

- Homeboy must have published releases with binary artifacts (uses `cargo-dist`)
- Extensions must be installable via `homeboy extension install`
- For WordPress: PHP must be available (use `php-version` input or set up separately)

## License

MIT
