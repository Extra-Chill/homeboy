'use strict';

const {
  WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
  normalizeCodeboxPublicResultEnvelope,
} = require('./codebox-artifact-contract');

function codeboxPublicResultEnvelope(result, options = {}) {
  return options.publicResultEnvelope || options.public_result_envelope || normalizeCodeboxPublicResultEnvelope(result, options);
}

function privateCodeboxRuntimeResultShapeNames(result = {}) {
  const names = [];
  if (result?.run?.agentResult) {
    names.push('run.agentResult');
  }
  if (result?.agentResult) {
    names.push('agentResult');
  }
  if (result?.agent_result) {
    names.push('agent_result');
  }
  if (result?.metadata?.agent_runtime) {
    names.push('metadata.agent_runtime');
  }
  if (result?.engine_data || result?.metadata?.engine_data) {
    names.push('engine_data');
  }
  return names;
}

function publicEnvelopeBoundaryDiagnostic(result, options = {}) {
  if (codeboxPublicResultEnvelope(result, options)) {
    return null;
  }
  const privateShapes = privateCodeboxRuntimeResultShapeNames(result);
  if (privateShapes.length === 0) {
    return null;
  }
  return {
    class: 'codebox.public_result_envelope_missing',
    message: 'WP Codebox result used private runtime fields without the canonical public artifact result envelope.',
    data: {
      required_schema: WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
      private_shapes: privateShapes,
    },
  };
}

module.exports = {
  codeboxPublicResultEnvelope,
  privateCodeboxRuntimeResultShapeNames,
  publicEnvelopeBoundaryDiagnostic,
};
