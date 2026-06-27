'use strict';

try {
	module.exports = require('../../agent-task-contracts/generic-agent-task-plan');
} catch (error) {
	if (error.code !== 'MODULE_NOT_FOUND') {
		throw error;
	}
	module.exports = require('../../../agent-task-contracts/generic-agent-task-plan');
}
