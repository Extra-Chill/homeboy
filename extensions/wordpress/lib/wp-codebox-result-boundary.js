'use strict';

const {
  WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
  normalizeCodeboxPublicResultEnvelope,
} = require('./wp-codebox-artifact-contract');

function codeboxPublicResultEnvelope(result, options = {}) {
  return options.publicResultEnvelope || options.public_result_envelope || normalizeCodeboxPublicResultEnvelope(result, options);
}

function publicEnvelopeBoundaryDiagnostic(result, options = {}) {
  if (codeboxPublicResultEnvelope(result, options)) {
    return null;
  }
  if (!result) {
    return null;
  }
  return {
    class: 'codebox.public_result_envelope_missing',
    message: 'WP Codebox result used private runtime fields without the canonical public artifact result envelope.',
    data: {
      required_schema: WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
    },
  };
}

module.exports = {
  codeboxPublicResultEnvelope,
  publicEnvelopeBoundaryDiagnostic,
};
