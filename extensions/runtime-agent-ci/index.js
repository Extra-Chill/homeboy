'use strict';

// Deprecated compatibility barrel. New imports should use the narrower
// ./generic-orchestration or ./provider-adapters package boundaries.
Object.assign(module.exports, require('./generic-orchestration'));
Object.assign(module.exports, require('./provider-adapters'));
