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

console.log('wordpress manifest settings smoke passed');
