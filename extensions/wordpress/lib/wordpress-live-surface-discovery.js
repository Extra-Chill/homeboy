'use strict';

/**
 * External dependencies
 */
const { execFile } = require('node:child_process');
const path = require('node:path');
const { promisify } = require('node:util');

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');
const { normalizeWordPressRuntimeSurfaceDiscovery } = require('./wordpress-runtime-surface-discovery');

const execFileAsync = promisify(execFile);

const WORDPRESS_LIVE_SURFACE_DISCOVERY_SOURCE = 'wordpress-live-surface-discovery';
const WORDPRESS_LIVE_SURFACE_TYPES = Object.freeze({
	rest_route: 'REST routes',
	admin_page: 'Admin pages',
	db_table: 'Database tables',
	frontend_url: 'Frontend URLs',
	block: 'Blocks',
});

function buildWordPressLiveSurfaceDiscoveryArtifact(input = {}, options = {}) {
	const payload = normalizeLiveSurfacePayload(input);
	const artifacts = [
		{ routes: payload.restRoutes },
		{ adminPages: payload.adminPages },
		{ tables: payload.databaseTables },
		{ frontendUrls: payload.frontendUrls },
		{ blocks: payload.blocks },
	];
	const unsupported = normalizeUnsupportedSurfaces(payload.unsupported, payload.supportedTypes);

	return normalizeWordPressRuntimeSurfaceDiscovery({
		id: input.id || options.id || 'wordpress-live-surface-discovery',
		generated_at: input.generated_at || input.generatedAt || options.generated_at || options.generatedAt || new Date(0).toISOString(),
		source: options.source || input.source || WORDPRESS_LIVE_SURFACE_DISCOVERY_SOURCE,
		artifacts,
		metadata: {
			...(isPlainObject(input.metadata) ? input.metadata : {}),
			collector_schema: input.schema || 'homeboy/wordpress-live-surface-discovery-raw/v1',
			unsupported_surfaces: unsupported,
		},
	}, options);
}

async function runWordPressLiveSurfaceDiscoveryWorkload(options = {}) {
	const rawDiscovery = await collectWordPressLiveSurfaceDiscovery(options);
	const discovery = buildWordPressLiveSurfaceDiscoveryArtifact(rawDiscovery, options);
	return {
		artifact: discovery,
		metrics: surfaceMetrics(discovery),
		metadata: {
			schema: discovery.schema,
			unsupported_surface_count: discovery.metadata.unsupported_surfaces.length,
			surface_count: discovery.surfaces.length,
		},
	};
}

async function collectWordPressLiveSurfaceDiscovery(options = {}) {
	if (typeof options.collector === 'function') {
		return await options.collector(options);
	}
	if (isPlainObject(options.runtime) && typeof options.runtime.discoverWordPressSurfaces === 'function') {
		return await options.runtime.discoverWordPressSurfaces(options);
	}
	if (isPlainObject(options.rawDiscovery)) {
		return options.rawDiscovery;
	}
	if (options.wpCli !== false && (options.wpCli || options.wpCliPath)) {
		return await collectWithWpCli(options);
	}
	return {
		schema: 'homeboy/wordpress-live-surface-discovery-raw/v1',
		unsupported: Object.keys(WORDPRESS_LIVE_SURFACE_TYPES).map((type) => unsupportedSurface(type, 'collector_unavailable', 'No runtime collector, WP-CLI command, or Codebox runtime discovery surface was supplied.')),
	};
}

async function collectWithWpCli(options = {}) {
	const wpCli = options.wpCli || options.wpCliPath || 'wp';
	const scriptPath = options.scriptPath || path.join(__dirname, '..', 'scripts', 'runtime', 'wordpress-live-surface-discovery.php');
	const args = Array.isArray(options.wpCliArgs) ? options.wpCliArgs : ['eval-file', scriptPath];
	const result = await execFileAsync(wpCli, args, {
		cwd: options.cwd || process.cwd(),
		maxBuffer: options.maxBuffer || 1024 * 1024 * 10,
		timeout: options.timeoutMs || 30000,
	});
	return parseJsonOutput(result.stdout, 'WP-CLI live surface discovery output');
}

function normalizeLiveSurfacePayload(input = {}) {
	const payload = isPlainObject(input) ? input : {};
	return {
		restRoutes: firstArray(payload.restRoutes, payload.rest_routes, payload.rest, payload.routes),
		adminPages: firstArray(payload.adminPages, payload.admin_pages, payload.admin),
		databaseTables: firstArray(payload.databaseTables, payload.database_tables, payload.database, payload.db, payload.tables),
		frontendUrls: firstArray(payload.frontendUrls, payload.frontend_urls, payload.frontend, payload.urls),
		blocks: firstArray(payload.blocks, payload.blockTypes, payload.block_types),
		unsupported: firstArray(payload.unsupported, payload.unsupportedSurfaces, payload.unsupported_surfaces),
		supportedTypes: firstArray(payload.supportedTypes, payload.supported_types),
	};
}

function firstArray(...values) {
	return values.find(Array.isArray) || [];
}

function normalizeUnsupportedSurfaces(rows, supportedTypes = []) {
	const supported = new Set(supportedTypes.map((type) => String(type || '').trim()).filter(Boolean));
	const unsupported = rows
		.map((row) => normalizeUnsupportedSurface(row))
		.filter((row) => row && !supported.has(row.type));
	return unsupported.sort((a, b) => a.type.localeCompare(b.type));
}

function normalizeUnsupportedSurface(row) {
	if (typeof row === 'string') {
		return unsupportedSurface(row, 'unsupported', `${WORDPRESS_LIVE_SURFACE_TYPES[row] || row} discovery is unsupported by this runtime.`);
	}
	if (!isPlainObject(row)) {
		return null;
	}
	const type = String(row.type || row.surface || row.kind || '').trim();
	if (!WORDPRESS_LIVE_SURFACE_TYPES[type]) {
		return null;
	}
	return unsupportedSurface(type, row.reason || row.code || 'unsupported', row.message || row.detail || `${WORDPRESS_LIVE_SURFACE_TYPES[type]} discovery is unsupported by this runtime.`);
}

function unsupportedSurface(type, reason, message) {
	return {
		type,
		label: WORDPRESS_LIVE_SURFACE_TYPES[type] || type,
		supported: false,
		reason: String(reason || 'unsupported'),
		message: String(message || ''),
	};
}

function surfaceMetrics(discovery) {
	const metrics = { wordpress_surface_count: discovery.surfaces.length };
	for (const type of Object.keys(WORDPRESS_LIVE_SURFACE_TYPES)) {
		metrics[`wordpress_surface_${type}_count`] = discovery.surfaces.filter((surface) => surface.type === type).length;
	}
	metrics.wordpress_surface_unsupported_count = discovery.metadata.unsupported_surfaces.length;
	return metrics;
}

function parseJsonOutput(output, label) {
	try {
		return JSON.parse(String(output || '').trim());
	} catch (error) {
		throw new Error(`${label} was not valid JSON: ${error.message}`);
	}
}

module.exports = {
	WORDPRESS_LIVE_SURFACE_DISCOVERY_SOURCE,
	WORDPRESS_LIVE_SURFACE_TYPES,
	buildWordPressLiveSurfaceDiscoveryArtifact,
	collectWordPressLiveSurfaceDiscovery,
	runWordPressLiveSurfaceDiscoveryWorkload,
};
