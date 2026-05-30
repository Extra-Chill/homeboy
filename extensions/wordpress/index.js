'use strict';

module.exports = {
	...require('./lib/admin-page-scenarios'),
	...require('./lib/wordpress-bootstrap-timeline'),
	...require('./lib/request-profiler'),
	...require('./lib/page-profiler'),
	...require('./lib/timing-correlator'),
	...require('./lib/codebox-memory-report'),
	...require('./lib/agent-terminal-actions'),
	...require('./lib/wp-codebox-apply-adapter'),
};
