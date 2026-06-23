'use strict';

const PROOF_PROFILES = {
  artifact_only: {
    proof_profile: 'artifact_only',
    preview_required: false,
    publication_required: false,
  },
  cook_to_pr: {
    proof_profile: 'cook_to_pr',
    preview_required: true,
    publication_required: true,
    publication_evidence: { kind: 'pull_request' },
  },
  none: {
    proof_profile: 'none',
    preview_required: false,
    publication_required: false,
    artifacts: [],
    required_evidence: [],
  },
};

function resolveControllerLoopProofPolicy(config = {}) {
  const explicitPolicy = {
    ...optionalObject(config.controller_loop_proof),
    ...optionalObject(config.controller_loop_proof_policy),
  };
  const profile = String(config.proof_profile || explicitPolicy.proof_profile || explicitPolicy.profile || 'artifact_only');
  const profilePolicy = PROOF_PROFILES[profile];
  if (!profilePolicy) {
    throw new Error(`Unsupported proof_profile: ${profile}. Expected one of: ${Object.keys(PROOF_PROFILES).join(', ')}`);
  }
  return {
    ...profilePolicy,
    ...explicitPolicy,
    proof_profile: profile,
  };
}

function optionalObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

module.exports = {
  PROOF_PROFILES,
  resolveControllerLoopProofPolicy,
};
