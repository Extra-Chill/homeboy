'use strict';

const {
  normalizeCodeboxArtifactDeclaration,
} = require('./codebox-artifact-contract');

function projectRuntimeConfig({ artifactDeclarations = [], manifestProjection = {}, env = process.env } = {}) {
  return {
    ...manifestProjection,
    artifact_declarations: artifactDeclarations
      .map((declaration, index) => normalizeCodeboxArtifactDeclaration(`artifact_${index + 1}`, declaration))
      .filter(Boolean),
    wp_config_defines: {
      ...(plainObject(manifestProjection.wp_config_defines) ? manifestProjection.wp_config_defines : {}),
      ...parseObject(env.EXTRA_WP_CONFIG_DEFINES || '{}'),
    },
  };
}

function parseObject(raw) {
  try {
    const value = JSON.parse(raw);
    return plainObject(value) ? value : {};
  } catch {
    return {};
  }
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = { projectRuntimeConfig };
