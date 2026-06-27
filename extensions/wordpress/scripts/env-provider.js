#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const extensionRoot = path.resolve(__dirname, '..');
const helperManifest = path.join(extensionRoot, 'lib', 'helper-manifest.js');

const env = {};

if (fs.existsSync(helperManifest)) {
	env.HOMEBOY_WORDPRESS_HELPER_MANIFEST = helperManifest;
}

process.stdout.write(`${JSON.stringify(env)}\n`);
