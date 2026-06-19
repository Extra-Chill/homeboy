'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_ARTIFACT_RELATIVE_PATH = 'wp-content/homeboy-external-http.jsonl';
const DEFAULT_PLUGIN_FILE_NAME = 'homeboy-external-http-guardrail.php';
const DEFAULT_BLOCK_RESPONSE = Object.freeze({
	code: 599,
	message: 'External HTTP blocked by Homeboy guardrail',
	body: '',
});

function normalizeSitePath(sitePath) {
	if (!sitePath || typeof sitePath !== 'string') {
		throw new TypeError('sitePath must be a non-empty string');
	}

	return path.resolve(sitePath);
}

function normalizeRelativePath(value, fallback) {
	const relativePath = value || fallback;
	if (typeof relativePath !== 'string' || relativePath.trim() === '') {
		throw new TypeError('artifactRelativePath must be a non-empty string');
	}
	if (path.isAbsolute(relativePath)) {
		throw new Error('artifactRelativePath must be relative to the WordPress site path');
	}

	const normalized = path.normalize(relativePath);
	if (normalized === '..' || normalized.startsWith(`..${path.sep}`)) {
		throw new Error('artifactRelativePath must stay inside the WordPress site path');
	}

	return normalized;
}

function normalizePluginFileName(value, fallback) {
	const pluginFileName = value || fallback;
	if (
		typeof pluginFileName !== 'string' ||
		pluginFileName.trim() === '' ||
		pluginFileName.includes('/') ||
		pluginFileName.includes('\\')
	) {
		throw new TypeError('pluginFileName must be a file name, not a path');
	}

	return pluginFileName;
}

function normalizeStringList(value, fallback, label) {
	const list = value === undefined ? fallback : value;
	if (!Array.isArray(list)) {
		throw new TypeError(`${label} must be an array`);
	}

	return [...new Set(list.map((item) => {
		if (typeof item !== 'string' || item.trim() === '') {
			throw new TypeError(`${label} entries must be non-empty strings`);
		}
		return item.trim().toLowerCase();
	}))];
}

function normalizeBlockResponse(value = {}) {
	const response = { ...DEFAULT_BLOCK_RESPONSE, ...value };
	const code = Number(response.code);
	if (!Number.isInteger(code) || code < 100 || code > 599) {
		throw new TypeError('blockResponse.code must be an HTTP status code');
	}
	if (typeof response.message !== 'string') {
		throw new TypeError('blockResponse.message must be a string');
	}
	if (typeof response.body !== 'string') {
		throw new TypeError('blockResponse.body must be a string');
	}

	return {
		code,
		message: response.message,
		body: response.body,
	};
}

function normalizePolicy(options = {}) {
	const allowlistDomains = normalizeStringList(options.allowlistDomains, [], 'allowlistDomains');
	const blockNetwork = options.blockNetwork !== undefined
		? Boolean(options.blockNetwork)
		: allowlistDomains.length > 0;

	return {
		allowlistDomains,
		blockNetwork,
		redactUrls: options.redactUrls !== false,
		blockResponse: normalizeBlockResponse(options.blockResponse),
	};
}

function resolveExternalHttpGuardrailPaths(sitePath, options = {}) {
	const root = normalizeSitePath(sitePath);
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH
	);
	const pluginFileName = normalizePluginFileName(options.pluginFileName, DEFAULT_PLUGIN_FILE_NAME);

	return {
		sitePath: root,
		muPluginsDir: path.join(root, 'wp-content', 'mu-plugins'),
		pluginPath: path.join(root, 'wp-content', 'mu-plugins', pluginFileName),
		artifactPath: path.join(root, artifactRelativePath),
		artifactRelativePath,
	};
}

function phpString(value) {
	return `'${String(value).replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

function phpBool(value) {
	return value ? 'true' : 'false';
}

function phpArray(values) {
	return `array( ${values.map(phpString).join(', ')} )`;
}

function redactExternalHttpGuardrailUrl(value) {
	if (typeof value !== 'string' || value === '') {
		return '';
	}

	try {
		const parsed = new URL(value);
		parsed.username = parsed.username ? 'redacted' : '';
		parsed.password = parsed.password ? 'redacted' : '';
		parsed.search = parsed.search ? '?redacted=1' : '';
		parsed.hash = '';
		return parsed.toString();
	} catch (_error) {
		return value.replace(/([?&][^=&#]+)=([^&#]*)/g, '$1=redacted').replace(/#.*$/, '');
	}
}

function generateExternalHttpGuardrailPlugin(options = {}) {
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH
	).replace(/\\/g, '/');
	const policy = normalizePolicy(options);

	return `<?php
/**
 * Plugin Name: Homeboy External HTTP Guardrail
 * Description: Temporary Homeboy MU-plugin for observing and controlling WordPress external HTTP calls.
 */

if ( ! defined( 'ABSPATH' ) ) {
	return;
}

if ( defined( 'HOMEBOY_EXTERNAL_HTTP_GUARDRAIL_LOADED' ) ) {
	return;
}

define( 'HOMEBOY_EXTERNAL_HTTP_GUARDRAIL_LOADED', true );

$homeboy_external_http_guardrail_start            = microtime( true );
$homeboy_external_http_guardrail_request_id       = substr( hash( 'sha256', ( $_SERVER['REQUEST_METHOD'] ?? 'CLI' ) . '|' . ( $_SERVER['REQUEST_URI'] ?? '' ) . '|' . $homeboy_external_http_guardrail_start ), 0, 16 );
$homeboy_external_http_guardrail_file             = ABSPATH . ${phpString(artifactRelativePath)};
$homeboy_external_http_guardrail_allowlist        = ${phpArray(policy.allowlistDomains)};
$homeboy_external_http_guardrail_block_network    = ${phpBool(policy.blockNetwork)};
$homeboy_external_http_guardrail_redact_urls      = ${phpBool(policy.redactUrls)};
$homeboy_external_http_guardrail_block_response   = array(
	'code'    => ${policy.blockResponse.code},
	'message' => ${phpString(policy.blockResponse.message)},
	'body'    => ${phpString(policy.blockResponse.body)},
);

if ( ! function_exists( 'homeboy_external_http_guardrail_redact_url' ) ) {
	function homeboy_external_http_guardrail_redact_url( $url ) {
		$parts = wp_parse_url( $url );
		if ( ! is_array( $parts ) || empty( $parts['host'] ) ) {
			return preg_replace( '/([?&][^=&#]+)=([^&#]*)/', '$1=redacted', preg_replace( '/#.*/', '', (string) $url ) );
		}

		$redacted = ( $parts['scheme'] ?? 'http' ) . '://';
		if ( ! empty( $parts['user'] ) ) {
			$redacted .= 'redacted@';
		}
		$redacted .= $parts['host'];
		if ( ! empty( $parts['port'] ) ) {
			$redacted .= ':' . $parts['port'];
		}
		$redacted .= $parts['path'] ?? '';
		if ( ! empty( $parts['query'] ) ) {
			$redacted .= '?redacted=1';
		}

		return $redacted;
	}
}

if ( ! function_exists( 'homeboy_external_http_guardrail_host_allowed' ) ) {
	function homeboy_external_http_guardrail_host_allowed( $host, $allowlist ) {
		$host = strtolower( trim( (string) $host, '.' ) );
		if ( '' === $host ) {
			return false;
		}

		foreach ( $allowlist as $domain ) {
			$domain = strtolower( trim( (string) $domain, '.' ) );
			if ( '' === $domain ) {
				continue;
			}
			$suffix = '.' . $domain;
			if ( $host === $domain || substr( $host, -strlen( $suffix ) ) === $suffix ) {
				return true;
			}
		}

		return false;
	}
}

if ( ! function_exists( 'homeboy_external_http_guardrail_write' ) ) {
	function homeboy_external_http_guardrail_write( $event, $data = array() ) {
		global $homeboy_external_http_guardrail_start, $homeboy_external_http_guardrail_request_id, $homeboy_external_http_guardrail_file;

		$entry = array(
			'v'          => 1,
			'event'      => $event,
			'timestamp'  => gmdate( 'c' ),
			't_ms'       => round( ( microtime( true ) - $homeboy_external_http_guardrail_start ) * 1000, 3 ),
			'request_id' => $homeboy_external_http_guardrail_request_id,
			'method'     => $_SERVER['REQUEST_METHOD'] ?? 'CLI',
			'uri'        => $_SERVER['REQUEST_URI'] ?? '',
			'data'       => $data,
		);

		$dir = dirname( $homeboy_external_http_guardrail_file );
		if ( ! is_dir( $dir ) ) {
			wp_mkdir_p( $dir );
		}

		file_put_contents(
			$homeboy_external_http_guardrail_file,
			wp_json_encode( $entry, JSON_UNESCAPED_SLASHES ) . PHP_EOL,
			FILE_APPEND | LOCK_EX
		);
	}
}

add_filter(
	'pre_http_request',
	static function ( $preempt, $parsed_args, $url ) {
		global $homeboy_external_http_guardrail_allowlist, $homeboy_external_http_guardrail_block_network, $homeboy_external_http_guardrail_redact_urls, $homeboy_external_http_guardrail_block_response;

		$host          = strtolower( (string) wp_parse_url( $url, PHP_URL_HOST ) );
		$allowed       = homeboy_external_http_guardrail_host_allowed( $host, $homeboy_external_http_guardrail_allowlist );
		$should_block  = $homeboy_external_http_guardrail_block_network && ! $allowed;
		$event_url     = $homeboy_external_http_guardrail_redact_urls ? homeboy_external_http_guardrail_redact_url( $url ) : $url;

		homeboy_external_http_guardrail_write(
			$should_block ? 'http.blocked' : 'http.allowed',
			array(
				'id'      => substr( hash( 'sha256', $url ), 0, 16 ),
				'url'     => $event_url,
				'host'    => $host,
				'method'  => $parsed_args['method'] ?? 'GET',
				'allowed' => $allowed,
				'blocked' => $should_block,
			)
		);

		if ( ! $should_block ) {
			return $preempt;
		}

		return array(
			'headers'  => array(),
			'body'     => $homeboy_external_http_guardrail_block_response['body'],
			'response' => array(
				'code'    => $homeboy_external_http_guardrail_block_response['code'],
				'message' => $homeboy_external_http_guardrail_block_response['message'],
			),
			'cookies'  => array(),
			'filename' => null,
		);
	},
	10,
	3
);
`;
}

function installWordPressExternalHttpGuardrail(sitePath, options = {}) {
	const paths = resolveExternalHttpGuardrailPaths(sitePath, options);
	fs.mkdirSync(paths.muPluginsDir, { recursive: true });
	fs.mkdirSync(path.dirname(paths.artifactPath), { recursive: true });

	if (options.clearArtifact !== false && fs.existsSync(paths.artifactPath)) {
		fs.unlinkSync(paths.artifactPath);
	}

	fs.writeFileSync(paths.pluginPath, generateExternalHttpGuardrailPlugin(options), 'utf8');
	return paths;
}

function uninstallWordPressExternalHttpGuardrail(sitePath, options = {}) {
	const paths = resolveExternalHttpGuardrailPaths(sitePath, options);
	if (fs.existsSync(paths.pluginPath)) {
		fs.unlinkSync(paths.pluginPath);
	}
	if (options.removeArtifact === true && fs.existsSync(paths.artifactPath)) {
		fs.unlinkSync(paths.artifactPath);
	}
	return paths;
}

function parseWordPressExternalHttpGuardrailJsonl(contents) {
	if (typeof contents !== 'string') {
		throw new TypeError('contents must be a string');
	}

	return contents
		.split(/\r?\n/)
		.map((line, index) => ({ line: line.trim(), lineNumber: index + 1 }))
		.filter(({ line }) => line !== '')
		.map(({ line, lineNumber }) => {
			try {
				return JSON.parse(line);
			} catch (error) {
				throw new Error(`Invalid WordPress external HTTP guardrail JSONL at line ${lineNumber}: ${error.message}`);
			}
		});
}

function collectWordPressExternalHttpGuardrailEvents(sitePath, options = {}) {
	const paths = resolveExternalHttpGuardrailPaths(sitePath, options);
	if (!fs.existsSync(paths.artifactPath)) {
		return [];
	}

	return parseWordPressExternalHttpGuardrailJsonl(fs.readFileSync(paths.artifactPath, 'utf8'));
}

function normalizeLimit(value, fallback) {
	const limit = Number(value ?? fallback);
	return Number.isFinite(limit) && limit >= 0 ? Math.floor(limit) : fallback;
}

function summarizeWordPressExternalHttpGuardrailEvents(events = [], options = {}) {
	if (!Array.isArray(events)) {
		throw new TypeError('events must be an array');
	}

	const urlFormatter = typeof options.formatUrl === 'function' ? options.formatUrl : redactExternalHttpGuardrailUrl;
	const hostCounts = new Map();
	let blockedCount = 0;
	let allowedCount = 0;

	for (const event of events) {
		const host = event?.data?.host || 'unknown';
		const current = hostCounts.get(host) || { host, count: 0, allowed: 0, blocked: 0 };
		current.count += 1;
		if (event?.data?.blocked || event?.event === 'http.blocked') {
			current.blocked += 1;
			blockedCount += 1;
		} else {
			current.allowed += 1;
			allowedCount += 1;
		}
		hostCounts.set(host, current);
	}

	const sampleLimit = normalizeLimit(options.sampleLimit, 20);

	return {
		event_count: events.length,
		allowed_count: allowedCount,
		blocked_count: blockedCount,
		hosts: [...hostCounts.values()].sort((a, b) => (b.count - a.count) || a.host.localeCompare(b.host)),
		samples: events.slice(0, sampleLimit).map((event) => ({
			event: event.event || '',
			request_id: event.request_id || 'unknown',
			host: event.data?.host || 'unknown',
			url: urlFormatter(event.data?.url || ''),
			method: event.data?.method || 'GET',
			blocked: Boolean(event.data?.blocked || event.event === 'http.blocked'),
		})),
	};
}

module.exports = {
	DEFAULT_EXTERNAL_HTTP_GUARDRAIL_ARTIFACT_RELATIVE_PATH: DEFAULT_ARTIFACT_RELATIVE_PATH,
	DEFAULT_EXTERNAL_HTTP_GUARDRAIL_BLOCK_RESPONSE: DEFAULT_BLOCK_RESPONSE,
	DEFAULT_EXTERNAL_HTTP_GUARDRAIL_PLUGIN_FILE_NAME: DEFAULT_PLUGIN_FILE_NAME,
	collectWordPressExternalHttpGuardrailEvents,
	generateExternalHttpGuardrailPlugin,
	installWordPressExternalHttpGuardrail,
	normalizeExternalHttpGuardrailPolicy: normalizePolicy,
	parseWordPressExternalHttpGuardrailJsonl,
	redactExternalHttpGuardrailUrl,
	resolveExternalHttpGuardrailPaths,
	summarizeWordPressExternalHttpGuardrailEvents,
	uninstallWordPressExternalHttpGuardrail,
};
