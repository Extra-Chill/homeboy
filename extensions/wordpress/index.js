'use strict';

module.exports = {
	...require('./lib/admin-page-scenarios'),
	...require('./lib/wordpress-bootstrap-timeline'),
	...require('./lib/request-profiler'),
	...require('./lib/page-profiler'),
	...require('./lib/wordpress-route-latency'),
	...require('./lib/block-quality'),
	...require('./lib/materialized-site-quality'),
	...require('./lib/editor-canvas-probes'),
	...require('./lib/fidelity-comparison'),
	...require('./lib/timing-correlator'),
	...require('./lib/helper-manifest'),
	...require('./lib/wordpress-helper-consumer'),
	...require('./lib/fixture-setup'),
	...require('./lib/codebox-memory-report'),
	...require('./lib/webperf-evidence-summary'),
	...require('./lib/benchmark-matrix-report'),
	...require('./lib/agent-terminal-actions'),
	...require('./lib/wp-codebox-apply-adapter'),
	...require('./lib/wp-codebox-recipe-helper'),
};
