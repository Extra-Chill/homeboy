'use strict';

module.exports = {
	...require('./lib/request-profiler'),
	...require('./lib/page-profiler'),
	...require('./lib/timing-correlator'),
};
