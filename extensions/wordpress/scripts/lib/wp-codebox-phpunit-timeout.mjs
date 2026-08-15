export const DEFAULT_WP_CODEBOX_PHPUNIT_TIMEOUT_SECONDS = 1440;

export function configuredWpCodeboxPhpunitTimeoutSeconds(environment = process.env, settings = {}) {
  const value = environment.HOMEBOY_WORDPRESS_PHPUNIT_TIMEOUT_SECONDS
    || settings.wp_codebox_phpunit_timeout_seconds
    || environment.HOMEBOY_TASK_TIMEOUT_SECONDS
    || DEFAULT_WP_CODEBOX_PHPUNIT_TIMEOUT_SECONDS;
  const seconds = Number(value);
  if (!Number.isSafeInteger(seconds) || seconds < 1) {
    throw new Error('WP Codebox PHPUnit timeout must be a positive integer number of seconds.');
  }
  return seconds;
}
