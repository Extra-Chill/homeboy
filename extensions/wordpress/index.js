'use strict';

module.exports = {
	...require('./lib/admin-page-scenarios'),
	...require('./lib/wordpress-bootstrap-timeline'),
	...require('./lib/request-profiler'),
	...require('./lib/page-profiler'),
	...require('./lib/block-quality'),
	...require('./lib/editor-canvas-probes'),
	...require('./lib/timing-correlator'),
	...require('./lib/helper-manifest'),
	...require('./lib/wordpress-helper-consumer'),
	...require('./lib/fixture-setup'),
	...require('./lib/codebox-memory-report'),
	...require('./lib/agent-terminal-actions'),
	...require('./lib/wp-codebox-apply-adapter'),
};
