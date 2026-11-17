#!/usr/bin/env node
'use strict';

// Builds the WordPress archive_install runtime commands from fpm-fastcgi.php,
// the single source for the FastCGI-to-PHP-FPM client:
//
//   after_switch_command                 inline opcache refresh (best-effort)
//   release_layout.precondition_command  opcache.revalidate_path check (fails closed)
//
// `node archive-runtime-commands.js --write` rewrites wordpress.json in place;
// `--check` exits non-zero when wordpress.json has drifted from the source.

const fs = require('node:fs');
const path = require('node:path');

const MANIFEST = path.resolve(__dirname, '..', '..', 'wordpress.json');
const SOURCE = path.resolve(__dirname, 'fpm-fastcgi.php');
const DELIMITER = 'HOMEBOY_OPCACHE_PHP';

function phpSource() {
	const source = fs.readFileSync(SOURCE, 'utf8').replace(/\n+$/, '');
	if (source.split('\n').includes(DELIMITER)) {
		throw new Error(`${SOURCE} must not contain a line equal to the heredoc delimiter`);
	}
	return source;
}

function afterSwitchCommand(source = phpSource()) {
	return `if command -v php >/dev/null 2>&1; then HOMEBOY_OPCACHE_TARGET='{{targetDir}}' php <<'${DELIMITER}' || true\n${source}\n${DELIMITER}\nelse echo 'opcache: php CLI not found; skipped'; fi`;
}

function preconditionCommand(source = phpSource()) {
	return `if ! command -v php >/dev/null 2>&1; then echo 'release precondition: php CLI not found; failing closed'; exit 1; fi\nHOMEBOY_FPM_MODE=release-precondition HOMEBOY_OPCACHE_TARGET='{{targetDir}}' php <<'${DELIMITER}'\n${source}\n${DELIMITER}`;
}

function releaseLayout(source = phpSource()) {
	return {
		release_root_dir: '.homeboy-releases',
		keep_previous: 2,
		keep_recent_minutes: 15,
		precondition_command: preconditionCommand(source),
	};
}

// wordpress.json is hand formatted; rewrite only the generated values.
function render(text) {
	const source = phpSource();
	const afterSwitch = `"after_switch_command": ${JSON.stringify(afterSwitchCommand(source))}`;
	const layout = `"release_layout": ${JSON.stringify(releaseLayout(source))}`;
	let next = text.replace(/"after_switch_command": "(?:[^"\\]|\\.)*"/g, afterSwitch);
	next = next.replace(/"release_layout": \{"release_root_dir"(?:[^"}]|"(?:[^"\\]|\\.)*")*\}/g, layout);
	// Insert release_layout after any after_switch_command that lacks one.
	next = next.replace(
		/^(\s*)("after_switch_command": "(?:[^"\\]|\\.)*",)\n(?!\s*"release_layout")/gm,
		(_, indent, line) => `${indent}${line}\n${indent}${layout},\n`
	);
	return next;
}

module.exports = { afterSwitchCommand, preconditionCommand, releaseLayout, render, MANIFEST };

if (require.main === module) {
	const current = fs.readFileSync(MANIFEST, 'utf8');
	const next = render(current);
	if (process.argv.includes('--write')) {
		fs.writeFileSync(MANIFEST, next);
		console.log(next === current ? 'wordpress.json already current' : 'wordpress.json updated');
	} else if (next !== current) {
		console.error('wordpress.json archive_install commands drifted from scripts/deploy/fpm-fastcgi.php; run node scripts/deploy/archive-runtime-commands.js --write');
		process.exit(1);
	} else {
		console.log('wordpress.json archive_install commands are current');
	}
}
