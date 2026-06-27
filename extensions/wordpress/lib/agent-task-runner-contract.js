'use strict';

// Compatibility re-export. New code should import from agent-task-contracts.
try {
	module.exports = require('../../agent-task-contracts/agent-task-runner-contract');
} catch (error) {
	if (error.code !== 'MODULE_NOT_FOUND') {
		throw error;
	}
	module.exports = require('../../../agent-task-contracts/agent-task-runner-contract');
}
