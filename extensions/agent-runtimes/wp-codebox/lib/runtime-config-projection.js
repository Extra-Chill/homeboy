'use strict';

const {
  normalizeCodeboxArtifactDeclaration,
} = require('./codebox-artifact-contract');

function projectRuntimeConfig({ artifactDeclarations = [], manifestProjection = {} } = {}) {
  return {
    ...manifestProjection,
    artifact_declarations: artifactDeclarations
      .map((declaration, index) => normalizeCodeboxArtifactDeclaration(`artifact_${index + 1}`, declaration))
      .filter(Boolean),
  };
}

module.exports = { projectRuntimeConfig };
