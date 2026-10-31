'use strict';

// Pins the HOME-relative runtime directories this extension declares for
// isolated gates (Extra-Chill/homeboy#15836) to the paths its runtime actually
// resolves. If a resolver default moves and this list does not, isolated
// Cook gates silently lose the managed WP Codebox install or the WordPress
// archive cache and run zero tests (Extra-Chill/homeboy-extensions#2908).

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const manifest = require('../wordpress.json');
const declared = manifest.isolated_home_paths;

assert.ok(Array.isArray(declared), 'wordpress.json declares isolated_home_paths');
for (const entry of declared) {
	assert.equal(typeof entry, 'string');
	assert.ok(entry.length > 0, 'paths are non-empty');
	assert.ok(!path.isAbsolute(entry), `${entry} is HOME-relative`);
	assert.ok(!entry.split('/').some((part) => part === '..' || part === '.' || part === ''), `${entry} has no traversal`);
}

// The managed WP Codebox install root the resolver defaults to.
const resolver = fs.readFileSync(path.join(__dirname, '../scripts/lib/wp-codebox-paths.sh'), 'utf8');
const installRoot = resolver.match(/HOMEBOY_WP_CODEBOX_INSTALL_DIR:-\$\{HOME\}\/([^}]+)\}/);
assert.ok(installRoot, 'wp-codebox-paths.sh still defaults the install root under HOME');
assert.ok(declared.includes(installRoot[1]), `declares the managed install root ${installRoot[1]}`);

// WordPress Playground's archive cache (WP_CODEBOX_PLAYGROUND_WORDPRESS_CACHE_DIR
// default: homedir()/.wordpress-playground). Without it, gates re-download
// WordPress core and fail on hosts that cannot reach downloads.w.org.
assert.ok(declared.includes('.wordpress-playground'), 'declares the Playground WordPress archive cache');

console.log(`ok - isolated_home_paths: ${declared.join(', ')}`);
