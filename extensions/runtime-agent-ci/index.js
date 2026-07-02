'use strict';

// Deprecated compatibility barrel. New imports should use ./provider-adapters;
// executor-neutral helpers live behind ./generic-orchestration only.
Object.assign(module.exports, require('./provider-adapters'));
