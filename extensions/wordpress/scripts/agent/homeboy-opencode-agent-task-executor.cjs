#!/usr/bin/env node
'use strict';

/**
 * Internal dependencies
 */
// Shared agent runtimes install beside the extensions directory, so a fixed
// relative require only resolves from a checkout (#12585).
const { requireAgentRuntimeModule } = require('../lib/agent-runtime-paths.cjs');

requireAgentRuntimeModule('opencode/scripts/agent/homeboy-opencode-agent-task-executor.cjs');
