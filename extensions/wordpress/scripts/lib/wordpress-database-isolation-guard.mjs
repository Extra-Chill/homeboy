// Fail-closed guard for WordPress PHPUnit/bench runs (Extra-Chill/homeboy#18886).
//
// A WordPress test install runs `wp-tests-config.php` and then `install.php`,
// which creates `<prefix>_*` tables in whatever database the DB_* constants
// name. The runner provisions that database itself (SQLite in the sandbox, or a
// declared `wp_codebox_database_service`), so any DB target that arrives from
// somewhere else is a way to install a test site into a live database:
//
// - `wp_config_defines` entries are written into the generated
//   wp-tests-config.php ahead of the runtime's own DB constants as
//   `if (!defined(...)) define(...)`, so a declared DB_NAME/DB_HOST is defined
//   before the runner's config and wins.
// - `bench_env` is `putenv()`'d into the sandbox before the config is written,
//   and the MySQL config path reads DB_HOST/DB_NAME/DB_USER/DB_PASSWORD with
//   getenv(), so a declared DB_* value replaces the provisioned service.
// - DB_* in the host environment is the same target leaking in from the
//   machine running Homeboy (a production host's shell, a CI service block).
//
// None of these has a safe interpretation, so the run is refused and the error
// names every offending source. Values are never echoed: they are credentials.

// Constants WordPress, the SQLite integration, and WP Codebox's managed config
// read to select the database. Prefix-matching DB_* would also catch unrelated
// names such as Homeboy's own DB_SERVICE_* passthrough, so the list is explicit.
export const DATABASE_TARGET_NAMES = Object.freeze([
  'DB_NAME',
  'DB_USER',
  'DB_PASSWORD',
  'DB_HOST',
  'DB_PORT',
  'DB_CHARSET',
  'DB_COLLATE',
  'DB_ENGINE',
  'DB_DIR',
  'DB_FILE',
  'CUSTOM_USER_TABLE',
  'CUSTOM_USER_META_TABLE',
]);

// Environment variables that point wp-phpunit at a different tests config,
// which then supplies its own DB constants.
export const TESTS_CONFIG_REDIRECT_NAMES = Object.freeze([
  'WP_PHPUNIT__TESTS_CONFIG',
  'WP_TESTS_CONFIG_FILE_PATH',
]);

const GUARDED_ENVIRONMENT_NAMES = new Set([...DATABASE_TARGET_NAMES, ...TESTS_CONFIG_REDIRECT_NAMES]);
const GUARDED_DEFINE_NAMES = new Set(DATABASE_TARGET_NAMES);

function isPlainObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/**
 * List every source that would point the run at a database it did not provision.
 *
 * Declared administration references (allowedEnvironmentNames) are consumed only
 * by the provisioning connector and stripped from every other child process, so
 * they are exempt.
 *
 * @param {Object}                           input
 * @param {Record<string, string|undefined>} input.environment               Host environment the runner inherited.
 * @param {Object}                           input.settings                  Parsed HOMEBOY_SETTINGS_JSON.
 * @param {string[]}                         [input.allowedEnvironmentNames] wp_codebox_database_service.secret_env references.
 * @return {string[]} Human-readable offending sources; empty when isolated.
 */
export function databaseIsolationViolations({ environment = {}, settings = {}, allowedEnvironmentNames = [] } = {}) {
  const allowed = new Set(allowedEnvironmentNames);
  const violations = [];
  for (const name of Object.keys(environment).sort()) {
    if (GUARDED_ENVIRONMENT_NAMES.has(name) && !allowed.has(name) && environment[name] !== undefined) {
      violations.push(`inherited host environment variable ${name}`);
    }
  }
  const benchEnv = isPlainObject(settings?.bench_env) ? settings.bench_env : {};
  for (const name of Object.keys(benchEnv).sort()) {
    if (GUARDED_ENVIRONMENT_NAMES.has(name)) {
      violations.push(`bench_env.${name} (exported into the sandbox, where the generated wp-tests-config reads it)`);
    }
  }
  const defines = isPlainObject(settings?.wp_config_defines) ? settings.wp_config_defines : {};
  for (const name of Object.keys(defines).sort()) {
    if (GUARDED_DEFINE_NAMES.has(name.toUpperCase())) {
      violations.push(`wp_config_defines.${name} (defined before the runner's wp-tests-config DB constants)`);
    }
  }
  return violations;
}

/**
 * Throw unless the run's database target is the one the runner provisions.
 *
 * @param {Object} input         See databaseIsolationViolations.
 * @param {string} [runnerLabel] Which runner is refusing, for the error text.
 */
export function assertIsolatedDatabaseTarget(input, runnerLabel = 'WordPress test runner') {
  const violations = databaseIsolationViolations(input);
  if (violations.length === 0) {
    return;
  }
  throw new Error(
    `${runnerLabel} refused to run: the WordPress database target is not the isolated database this runner provisions, `
    + 'so a test install could write into a live site database. '
    + `Offending source${violations.length === 1 ? '' : 's'}: ${violations.join('; ')}. `
    + 'Unset these (the runner provisions SQLite, or declare wp_codebox_database_service for MySQL); '
    + 'there is no fallback to a host database.',
  );
}
