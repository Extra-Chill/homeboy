'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

const RUNTIME_OVERLAY_PROFILE_SCHEMA = 'homeboy/runtime-overlay-profile/v1';
const RUNTIME_OVERLAY_PROFILE_FAILURE_CLASS = 'codebox.preflight.runtime_overlay_profile_invalid';
const COMMIT_SHA_PATTERN = /^[0-9a-f]{40,64}$/i;

class RuntimeOverlayProfileError extends Error {
  constructor(diagnostics) {
    super(diagnostics[0]?.message || 'Invalid runtime overlay profile.');
    this.name = 'RuntimeOverlayProfileError';
    this.diagnostics = diagnostics;
  }
}

function validateRuntimeOverlayProfiles(value, overlays = [], options = {}) {
  const profiles = normalizeArray(value);
  const diagnostics = [];
  const profileIds = new Set();

  profiles.forEach((profile, index) => {
    const prefix = `runtime_overlay_profiles[${index}]`;
    if (!isPlainObject(profile)) {
      diagnostics.push(diagnostic(index, prefix, 'entry', 'Runtime overlay profiles must be objects.'));
      return;
    }
    if (profile.schema !== RUNTIME_OVERLAY_PROFILE_SCHEMA) {
      diagnostics.push(diagnostic(index, `${prefix}.schema`, 'schema', `Expected ${RUNTIME_OVERLAY_PROFILE_SCHEMA}.`));
    }
    if (typeof profile.id !== 'string' || !profile.id.trim() || profileIds.has(profile.id)) {
      diagnostics.push(diagnostic(index, `${prefix}.id`, 'id', 'Profile ids must be unique non-empty strings.'));
    }
    profileIds.add(profile.id);
    const repository = plainObject(profile.repository);
    if (typeof repository.identity !== 'string' || !repository.identity.trim()) {
      diagnostics.push(diagnostic(index, `${prefix}.repository.identity`, 'repository.identity', 'Repository identity is required.'));
    }
    if (!COMMIT_SHA_PATTERN.test(repository.sha || '') || repository.ref !== repository.sha) {
      diagnostics.push(diagnostic(index, `${prefix}.repository`, 'repository.ref', 'Proof-bearing profiles require repository.ref and repository.sha to be the same full commit SHA.'));
    }
    if (typeof profile.source !== 'string' || !profile.source.trim()) {
      diagnostics.push(diagnostic(index, `${prefix}.source`, 'source', 'Source checkout path is required.'));
    }
    if (typeof profile.target !== 'string' || !profile.target.startsWith('/')) {
      diagnostics.push(diagnostic(index, `${prefix}.target`, 'target', 'Target path must be absolute.'));
    }
    const evidence = plainObject(profile.preparation_evidence);
    const checkout = plainObject(evidence.checkout);
    if (checkout.repository_identity !== repository.identity || checkout.ref !== repository.ref || checkout.sha !== repository.sha || checkout.clean !== true || !Array.isArray(evidence.probes)) {
      diagnostics.push(diagnostic(index, `${prefix}.preparation_evidence`, 'preparation_evidence', 'Preparation evidence must bind the declared repository identity, ref, SHA, clean checkout, and probes array.'));
    }
    if (!Array.isArray(profile.required_capabilities) || profile.required_capabilities.some((capability) => typeof capability !== 'string' || !capability.trim())) {
      diagnostics.push(diagnostic(index, `${prefix}.required_capabilities`, 'required_capabilities', 'Required capabilities must be an array of non-empty strings.'));
    }
  });

  const normalizedOverlays = normalizeArray(overlays).map((overlay, index) => {
    if (!plainObject(overlay)) return overlay;
    const profileId = overlay.profile_id || overlay.profileId;
    if (!profileId) {
      if (options.proofBearing) {
        diagnostics.push(diagnostic(index, `runtime_overlays[${index}].profile_id`, 'profile_id', 'Proof-bearing runtime overlays must declare a profile_id.'));
      }
      return overlay;
    }
    const profile = profiles.find((candidate) => candidate?.id === profileId);
    if (!profile) {
      diagnostics.push(diagnostic(index, `runtime_overlays[${index}].profile_id`, 'profile_id', `No runtime overlay profile exists for ${profileId}.`));
      return overlay;
    }
    if (overlay.source && overlay.source !== profile.source) {
      diagnostics.push(diagnostic(index, `runtime_overlays[${index}].source`, 'source', `Overlay source must match profile ${profileId}.`));
    }
    if (overlay.target && overlay.target !== profile.target) {
      diagnostics.push(diagnostic(index, `runtime_overlays[${index}].target`, 'target', `Overlay target must match profile ${profileId}.`));
    }
    // Profiles own the mount coordinates so downstream defaults cannot weaken them.
    return { ...overlay, source: profile.source, target: profile.target, profile_id: profile.id };
  });

  if (options.proofBearing) {
    profiles.forEach((profile, index) => {
      const bindings = normalizedOverlays.filter((overlay) => overlay?.profile_id === profile?.id);
      if (bindings.length !== 1) {
        diagnostics.push(diagnostic(index, `runtime_overlay_profiles[${index}].id`, 'id', 'Proof-bearing profiles must bind to exactly one runtime overlay.'));
      }
    });
  }

  if (diagnostics.length > 0) throw new RuntimeOverlayProfileError(diagnostics);
  return { profiles, overlays: normalizedOverlays };
}

function runtimeOverlayProfileReadinessDiagnostics(profiles = []) {
  return normalizeArray(profiles).flatMap((profile, index) => {
    if (profile?.schema !== RUNTIME_OVERLAY_PROFILE_SCHEMA) return [];
    if (!plainObject(profile)) return [];
    const source = resolveRealPath(profile.source);
    if (!source) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].source`, 'source', 'Source checkout does not exist.')];
    }
    const resolvedSha = gitSha(source);
    if (resolvedSha !== profile.repository?.sha) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].repository.sha`, 'repository.sha', 'Source checkout does not resolve to the declared commit SHA.', { resolved_sha: resolvedSha })];
    }
    if (!gitClean(source)) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].source`, 'source', 'Source checkout has uncommitted changes.')];
    }
    const resolvedIdentity = gitIdentity(source);
    if (resolvedIdentity !== normalizeRepositoryIdentity(profile.repository?.identity)) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].repository.identity`, 'repository.identity', 'Source checkout does not resolve to the declared repository identity.', { resolved_identity: resolvedIdentity })];
    }
    const probes = profile.preparation_evidence?.probes || [];
    const missing = (profile.required_capabilities || []).filter((capability) => !probes.some((probe) => probe?.capability === capability));
    if (missing.length > 0) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].required_capabilities`, 'required_capabilities', `Preparation evidence is missing probes for required capabilities: ${missing.join(', ')}.`, { missing_capabilities: missing })];
    }
    const failedProbe = probes.find((probe) => !capabilityProbePasses(probe, source));
    if (failedProbe) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].preparation_evidence.probes`, 'probes', `Capability probe failed: ${failedProbe.capability || '(unnamed)'}.`)];
    }
    // Probes are executable source code. Re-establish the immutable checkout
    // boundary after they finish and before the runtime can consume the overlay.
    const finalSha = gitSha(source);
    if (finalSha !== profile.repository?.sha) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].repository.sha`, 'repository.sha', 'Source checkout changed while capability probes ran.', { resolved_sha: finalSha })];
    }
    if (!gitClean(source)) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].source`, 'source', 'Source checkout changed while capability probes ran.')];
    }
    if (gitIdentity(source) !== normalizeRepositoryIdentity(profile.repository?.identity)) {
      return [diagnostic(index, `runtime_overlay_profiles[${index}].repository.identity`, 'repository.identity', 'Source checkout identity changed while capability probes ran.')];
    }
    return [];
  });
}

function runtimeOverlayProfileEvidence(profiles = []) {
  return normalizeArray(profiles).filter((profile) => profile?.schema === RUNTIME_OVERLAY_PROFILE_SCHEMA).map((profile) => ({
    id: profile.id,
    repository: { identity: profile.repository.identity, sha: profile.repository.sha },
    target: profile.target,
    required_capabilities: profile.required_capabilities,
    preparation_evidence: profile.preparation_evidence,
  }));
}

function gitSha(source) {
  try {
    return execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
  } catch {
    return '';
  }
}

function gitClean(source) {
  try {
    return execFileSync('git', ['-C', source, 'status', '--porcelain'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim() === '';
  } catch {
    return false;
  }
}

function gitIdentity(source) {
  try {
    return normalizeRepositoryIdentity(execFileSync('git', ['-C', source, 'remote', 'get-url', 'origin'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim());
  } catch {
    return '';
  }
}

function normalizeRepositoryIdentity(value) {
  return String(value || '').trim().replace(/^git@([^:]+):/i, '$1/').replace(/^[a-z]+:\/\//i, '').replace(/\.git$/i, '').replace(/^\/+|\/+$/g, '').toLowerCase();
}

function capabilityProbePasses(probe, source) {
  if (!plainObject(probe) || typeof probe.capability !== 'string' || !probe.capability.trim() || !Array.isArray(probe.command) || probe.command.length === 0 || probe.command.some((entry) => typeof entry !== 'string' || !entry)) {
    return false;
  }
  const executable = probe.command[0];
  if (!executable.startsWith('./')) {
    return false;
  }
  const executablePath = resolveRealPath(path.resolve(source, executable));
  if (!executablePath || !pathInside(source, executablePath)) {
    return false;
  }
  try {
    execFileSync(executablePath, probe.command.slice(1), { cwd: source, stdio: 'ignore', timeout: Number.isSafeInteger(probe.timeout_ms) ? probe.timeout_ms : 10000 });
    return true;
  } catch {
    return false;
  }
}

function pathInside(parent, candidate) {
  const relative = path.relative(path.resolve(parent), path.resolve(candidate));
  return relative === '' || (Boolean(relative) && !relative.startsWith('..') && !path.isAbsolute(relative));
}

function resolveRealPath(value) {
  try {
    return fs.realpathSync(resolvePath(value));
  } catch {
    return '';
  }
}

function diagnostic(index, field, offendingField, message, data = {}) {
  return {
    class: RUNTIME_OVERLAY_PROFILE_FAILURE_CLASS,
    message: `Invalid runtime overlay profile at runtime_overlay_profiles[${index}]: ${message}`,
    data: { overlay_profile_index: index, field, offending_field: offendingField, ...data },
  };
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : (value === undefined || value === null ? [] : [value]);
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function isPlainObject(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function resolvePath(value) {
  return path.resolve(String(value || ''));
}

module.exports = {
  RUNTIME_OVERLAY_PROFILE_FAILURE_CLASS,
  RUNTIME_OVERLAY_PROFILE_SCHEMA,
  RuntimeOverlayProfileError,
  runtimeOverlayProfileEvidence,
  runtimeOverlayProfileReadinessDiagnostics,
  validateRuntimeOverlayProfiles,
};
