'use strict';

module.exports = {
	...require('./lib/admin-page-scenarios'),
	...require('./lib/request-profiler'),
	...require('./lib/page-profiler'),
	...require('./lib/timing-correlator'),
	...require('./lib/agent-terminal-actions'),
	...require('./lib/wp-codebox-apply-adapter'),
};
