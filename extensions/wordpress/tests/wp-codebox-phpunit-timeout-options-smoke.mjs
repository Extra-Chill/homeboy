import { strict as assert } from 'node:assert';

import {
  DEFAULT_WP_CODEBOX_PHPUNIT_TIMEOUT_SECONDS,
  configuredWpCodeboxPhpunitTimeoutSeconds,
} from '../scripts/lib/wp-codebox-phpunit-timeout.mjs';

assert.equal(configuredWpCodeboxPhpunitTimeoutSeconds({}, {}), DEFAULT_WP_CODEBOX_PHPUNIT_TIMEOUT_SECONDS);
assert.equal(configuredWpCodeboxPhpunitTimeoutSeconds({ HOMEBOY_TASK_TIMEOUT_SECONDS: '1200' }, {}), 1200);
assert.equal(configuredWpCodeboxPhpunitTimeoutSeconds({}, { wp_codebox_phpunit_timeout_seconds: 900 }), 900);
assert.equal(configuredWpCodeboxPhpunitTimeoutSeconds({ HOMEBOY_TASK_TIMEOUT_SECONDS: '1200' }, { wp_codebox_phpunit_timeout_seconds: 900 }), 900);
assert.equal(configuredWpCodeboxPhpunitTimeoutSeconds({ HOMEBOY_WORDPRESS_PHPUNIT_TIMEOUT_SECONDS: '600', HOMEBOY_TASK_TIMEOUT_SECONDS: '1200' }, { wp_codebox_phpunit_timeout_seconds: 900 }), 600);
for (const value of ['0', '-1', '1.5', 'nope']) {
  assert.throws(() => configuredWpCodeboxPhpunitTimeoutSeconds({ HOMEBOY_WORDPRESS_PHPUNIT_TIMEOUT_SECONDS: value }, {}), /positive integer/);
}

console.log('WP Codebox PHPUnit timeout option smoke passed.');
