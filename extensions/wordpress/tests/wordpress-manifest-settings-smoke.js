'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const manifest = JSON.parse(
	fs.readFileSync(path.resolve(__dirname, '..', 'wordpress.json'), 'utf8')
);

const deployHooks = manifest.hooks['post:deploy'];
assert.equal(deployHooks.length, 1, 'post:deploy retains ownership warning; archive install refreshes opcache inline');
assert.ok(deployHooks[0].includes('DEPLOY_OWNER='), 'deploy preserves the ownership check');
assert.ok(deployHooks[0].includes('root:root'), 'deploy preserves the root:root warning');
assert.ok(
	deployHooks[0].includes("stat -L -c '%U:%G'") && deployHooks[0].includes("stat -L -f '%Su:%Sg'"),
	'the ownership check follows a release-layout symlink to the release tree'
);

// Release layout (homeboy#18365): plugins and themes opt in, gated by the
// PHP-FPM opcache.revalidate_path precondition; the generic deploy layer stays
// runtime-agnostic.
const archivePolicies = manifest.deploy.archive_install;
assert.deepEqual(archivePolicies.map((policy) => policy.path_pattern), ['/wp-content/plugins/', '/wp-content/themes/']);
for (const policy of archivePolicies) {
	const layout = policy.release_layout;
	assert.ok(layout, `${policy.path_pattern} opts in to the release layout`);
	assert.equal(layout.release_root_dir, '.homeboy-releases');
	assert.match(layout.release_root_dir, /^\./, 'WordPress skips dot-prefixed directories when scanning plugins and themes');
	assert.equal(layout.keep_previous, 2);
	assert.equal(layout.keep_recent_minutes, 15);
	assert.ok(layout.precondition_command.includes('HOMEBOY_FPM_MODE=release-precondition'));
	assert.ok(layout.precondition_command.includes("ini_get('opcache.revalidate_path')"));
	assert.ok(layout.precondition_command.includes('failing closed'));
	assert.ok(policy.after_switch_command, `${policy.path_pattern} keeps the inline opcache refresh`);
}
assert.ok(
	deployHooks.every((hook) => !/\bwp\s+cache\s+flush\b/.test(hook)),
	'component deploy must not flush the object cache'
);

const settingIds = new Set(manifest.settings.map((setting) => setting.id));

assert.ok(
	settingIds.has('wp_codebox_bin'),
	'wordpress manifest declares wp_codebox_bin so --setting wp_codebox_bin is accepted'
);

for (const settingId of ['package_artifacts', 'package_excludes', 'package_profile', 'additional_package_profiles', 'standalone_php_test_paths', 'wp_codebox_extra_themes', 'wp_codebox_dependency_overlays', 'wp_codebox_multisite', 'wp_codebox_database_service', 'wordpress_multisite_synthetic_fixture', 'wordpress_runtime_php_version', 'wordpress_runtime_workload_plugin_slug']) {
	assert.ok(settingIds.has(settingId), `wordpress manifest declares ${settingId}`);
}

for (const legacySettingId of ['wp_codebox_blueprint', 'wp_codebox_workloads', 'wp_codebox_recipe_prepare_steps', 'wp_codebox_recipe_post_steps']) {
	assert.equal(settingIds.has(legacySettingId), false, `wordpress manifest no longer accepts ${legacySettingId}`);
}

assert.deepEqual(manifest.test.secret_env_projections, [
	{
		when: {
			path: ['wp_codebox_database_service', 'provider'],
			equals: 'external',
		},
		names_path: ['wp_codebox_database_service', 'secret_env'],
	},
], 'external WP Codebox database identity names are projected through Homeboy test secret resolution');

const fuzzEnv = new Set(manifest.fuzz.env);
for (const envKey of ['HOMEBOY_SETTINGS_JSON', 'HOMEBOY_SETTINGS_WP_CODEBOX_BIN', 'HOMEBOY_WP_CODEBOX_BIN', 'WP_CODEBOX_BIN']) {
	assert.ok(
		fuzzEnv.has(envKey),
		`wordpress fuzz runner forwards ${envKey} so WP Codebox binary settings reach offloaded runs`
	);
}

console.log('wordpress manifest settings smoke passed');
