'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const manifest = JSON.parse(
	fs.readFileSync(path.resolve(__dirname, '..', 'wordpress.json'), 'utf8')
);

const settingIds = new Set(manifest.settings.map((setting) => setting.id));

assert.ok(
	settingIds.has('wp_codebox_bin'),
	'wordpress manifest declares wp_codebox_bin so --setting wp_codebox_bin is accepted'
);

for (const settingId of ['package_artifacts', 'package_excludes', 'standalone_php_test_paths', 'wp_codebox_extra_themes', 'wp_codebox_dependency_overlays', 'wp_codebox_multisite', 'wp_codebox_database_service', 'wordpress_multisite_synthetic_fixture', 'wordpress_runtime_php_version', 'wordpress_runtime_workload_plugin_slug']) {
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
