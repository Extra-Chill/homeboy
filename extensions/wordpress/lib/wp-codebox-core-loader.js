'use strict';

/**
 * External dependencies
 */
const path = require('node:path');
const { pathToFileURL } = require('node:url');

function coreModuleSpecifier(options = {}) {
  const explicit = options.wpCodeboxCoreModule || options.coreModule || process.env.WP_CODEBOX_CORE_MODULE;
  if (!explicit) {
    return '@automattic/wp-codebox-core';
  }
  if (explicit.startsWith('file:') || explicit.startsWith('node:')) {
    return explicit;
  }
  if (explicit.startsWith('.') || explicit.startsWith('/') || explicit.includes(path.sep)) {
    return pathToFileURL(path.resolve(explicit)).href;
  }
  return explicit;
}

async function loadWpCodeboxCore(options = {}) {
  const specifier = coreModuleSpecifier(options);
  try {
    return await import(specifier);
  } catch (error) {
    if (options.required) {
      throw error;
    }
    return null;
  }
}

async function loadWpCodeboxCoreFunction(name, options = {}) {
  const core = await loadWpCodeboxCore(options);
  const fn = core && core[name];
  return typeof fn === 'function' ? fn : null;
}

module.exports = {
  coreModuleSpecifier,
  loadWpCodeboxCore,
  loadWpCodeboxCoreFunction,
};
