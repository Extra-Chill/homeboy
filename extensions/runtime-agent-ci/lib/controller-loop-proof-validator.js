'use strict';

function validateControllerLoopProof(input = {}) {
  const spec = optionalObject(input.spec || input.controller || input.loop_spec);
  const proof = optionalObject(input.proof || input.run || input.result);
  const policy = {
    ...optionalObject(spec.policy),
    ...optionalObject(input.policy),
  };
  const failures = [];
  const warnings = [];
  const artifactDeclarations = normalizeArtifactDeclarations(spec.artifacts || spec.artifact_declarations || policy.artifacts || policy.artifact_declarations);
  const artifacts = collectArtifacts(proof);
  const evidence = collectEvidence(proof);
  const events = collectEvents(proof);
  const iterations = collectIterations(proof);
  const artifactResults = [];

  const status = proof.status || proof.result?.status || proof.outcome?.status || '';
  const allowedStatuses = normalizeArray(policy.allowed_statuses || spec.allowed_statuses || spec.statuses);
  if (allowedStatuses.length > 0 && !allowedStatuses.includes(status)) {
    failures.push(failure('proof.status_rejected', `Proof status is not allowed: ${status || '(missing)'}`, { status, allowed_statuses: allowedStatuses }));
  }

  for (const declaration of artifactDeclarations) {
    const match = findArtifact(artifacts, declaration.id);
    const required = declaration.required !== false && (declaration.required === true || policy.require_declared_artifacts === true);
    artifactResults.push({
      id: declaration.id,
      required,
      present: Boolean(match),
      artifact: match || null,
    });
    if (required && !match) {
      failures.push(failure('artifact.required_missing', `Required artifact is missing: ${declaration.id}`, { artifact_id: declaration.id }));
      continue;
    }
    if (match) {
      validateReviewerFacingUrls({
        ref: match,
        failures,
        policy,
        reviewerFacing: declaration.reviewer_facing,
        failureClass: 'artifact.local_reviewer_evidence',
        label: declaration.id,
      });
      if (declaration.durable_url_required === true && !durableUrl(refUrl(match))) {
        failures.push(failure('artifact.durable_url_missing', `Artifact must include a durable non-local URL: ${declaration.id}`, { artifact_id: declaration.id }));
      }
    }
  }

  for (const requirement of normalizeEvidenceRequirements(policy.required_evidence || spec.required_evidence)) {
    const match = findEvidence(evidence, requirement);
    if (!match) {
      failures.push(failure('evidence.required_missing', `Required evidence is missing: ${requirementLabel(requirement)}`, { requirement }));
      continue;
    }
    validateReviewerFacingUrls({
      ref: match,
      failures,
      policy,
      reviewerFacing: requirement.reviewer_facing,
      failureClass: 'evidence.local_reviewer_evidence',
      label: requirementLabel(requirement),
    });
    if (requirement.durable_url_required === true && !durableUrl(refUrl(match))) {
      failures.push(failure('evidence.durable_url_missing', `Evidence must include a durable non-local URL: ${requirementLabel(requirement)}`, { requirement }));
    }
  }

  validateSpecialEvidenceRequirement({
    required: policy.preview_required === true || policy.require_preview === true,
    requirement: policy.preview_evidence || spec.preview_evidence || { kind: 'preview' },
    evidence,
    failures,
    policy,
    failureClass: 'preview.required_missing',
    message: 'Preview materialization evidence is required.',
  });
  validateSpecialEvidenceRequirement({
    required: policy.pr_required === true || policy.require_pr === true || policy.publication_required === true || policy.require_publication === true,
    requirement: policy.publication_evidence || policy.pr_evidence || spec.publication_evidence || spec.pr_evidence || { kind: 'pull_request' },
    evidence,
    failures,
    policy,
    failureClass: 'publication.required_missing',
    message: 'PR or publication evidence is required.',
  });

  for (const ref of evidence) {
    validateReviewerFacingUrls({
      ref,
      failures,
      policy,
      reviewerFacing: ref.reviewer_facing,
      failureClass: 'evidence.local_reviewer_evidence',
      label: evidenceLabel(ref),
    });
  }

  const iterationCount = explicitIterationCount(proof, iterations);
  const maxIterations = positiveInteger(policy.max_iterations || spec.max_iterations || spec.loop?.max_iterations);
  if (maxIterations && iterationCount > maxIterations) {
    failures.push(failure('loop.max_iterations_exceeded', `Iteration count ${iterationCount} exceeds maximum ${maxIterations}.`, { iteration_count: iterationCount, max_iterations: maxIterations }));
  }

  const stopReason = stopReasonFromProof(proof, iterations);
  const allowedStopReasons = normalizeArray(policy.allowed_stop_reasons || spec.allowed_stop_reasons || spec.stop_reasons);
  if ((policy.stop_reason_required === true || allowedStopReasons.length > 0) && !stopReason) {
    failures.push(failure('loop.stop_reason_missing', 'Stop reason evidence is required.', {}));
  } else if (stopReason && allowedStopReasons.length > 0 && !allowedStopReasons.includes(stopReason)) {
    failures.push(failure('loop.stop_reason_rejected', `Stop reason is not allowed: ${stopReason}`, { stop_reason: stopReason, allowed_stop_reasons: allowedStopReasons }));
  }

  if (policy.event_lineage_required === true || policy.require_event_lineage === true || spec.event_lineage_required === true) {
    validateEventLineage({ proof, iterations, events, failures, warnings });
  }

  return {
    schema: 'homeboy/controller-loop-proof-validation/v1',
    valid: failures.length === 0,
    failure_count: failures.length,
    warning_count: warnings.length,
    failures,
    warnings,
    summary: {
      artifact_count: artifacts.length,
      evidence_count: evidence.length,
      event_count: events.length,
      iteration_count: iterationCount,
      stop_reason: stopReason || '',
    },
    artifact_results: artifactResults,
  };
}

function assertControllerLoopProof(input = {}) {
  const report = validateControllerLoopProof(input);
  if (!report.valid) {
    const error = new Error(`controller loop proof validation failed: ${report.failures.map((item) => item.message).join('; ')}`);
    error.report = report;
    throw error;
  }
  return report;
}

function validateSpecialEvidenceRequirement(options) {
  if (!options.required) {
    return;
  }
  const match = findEvidence(options.evidence, options.requirement);
  if (!match) {
    options.failures.push(failure(options.failureClass, options.message, { requirement: options.requirement }));
    return;
  }
  validateReviewerFacingUrls({
    ref: match,
    failures: options.failures,
    policy: options.policy,
    reviewerFacing: true,
    failureClass: 'evidence.local_reviewer_evidence',
    label: requirementLabel(options.requirement),
  });
}

function validateReviewerFacingUrls(options) {
  const reviewerFacing = options.reviewerFacing !== false;
  if (!reviewerFacing || options.policy.allow_local_reviewer_evidence === true) {
    return;
  }
  const url = refUrl(options.ref);
  if (!url) {
    return;
  }
  if (localOnlyUrl(url)) {
    options.failures.push(failure(options.failureClass, `Reviewer-facing evidence must use a durable non-local URL: ${options.label}`, { url }));
  }
}

function validateEventLineage(options) {
  if (options.events.length === 0) {
    options.failures.push(failure('event_lineage.missing', 'Event lineage is required but no events were provided.', {}));
    return;
  }
  const eventIds = new Set(options.events.map((event) => event.id || event.event_id).filter(Boolean));
  const duplicateIds = duplicateValues(options.events.map((event) => event.id || event.event_id).filter(Boolean));
  for (const id of duplicateIds) {
    options.failures.push(failure('event_lineage.duplicate_event', `Event lineage contains duplicate event id: ${id}`, { event_id: id }));
  }
  for (const event of options.events) {
    const parentId = event.parent_id || event.parentId || event.previous_event_id || event.previousEventId;
    if (parentId && !eventIds.has(parentId)) {
      options.failures.push(failure('event_lineage.missing_parent', `Event references a missing parent event: ${parentId}`, { event_id: event.id || event.event_id || '', parent_id: parentId }));
    }
  }
  for (const iteration of options.iterations) {
    const eventId = iteration.event_id || iteration.eventId || iteration.event?.id || iteration.event?.event_id;
    if (!eventId) {
      options.failures.push(failure('event_lineage.iteration_event_missing', `Iteration ${iteration.iteration ?? iteration.index ?? '(unknown)'} is missing event lineage evidence.`, { iteration: iteration.iteration ?? iteration.index ?? null }));
    } else if (!eventIds.has(eventId)) {
      options.failures.push(failure('event_lineage.iteration_event_unknown', `Iteration references an unknown event: ${eventId}`, { event_id: eventId }));
    }
  }
  if (options.iterations.length === 0) {
    options.warnings.push({ class: 'event_lineage.no_iterations', message: 'Event lineage was provided without iteration records.', data: {} });
  }
}

function collectArtifacts(proof) {
  return [
    ...normalizeArray(proof.artifacts),
    ...normalizeArray(proof.proof_artifacts),
    ...normalizeArray(proof.evidence_envelope?.artifacts),
    ...normalizeArray(proof.iterations || proof.loop?.iterations).flatMap((iteration) => normalizeArray(iteration?.artifacts)),
    ...normalizeArray(proof.evidence_envelope?.iterations).flatMap((iteration) => normalizeArray(iteration?.artifacts)),
  ].filter(isObject);
}

function collectEvidence(proof) {
  return [
    ...normalizeArray(proof.evidence),
    ...normalizeArray(proof.evidence_refs),
    ...normalizeArray(proof.evidence_envelope?.evidence),
    ...normalizeArray(proof.evidence_envelope?.evidence_refs),
    ...normalizeArray(proof.iterations || proof.loop?.iterations).flatMap((iteration) => normalizeArray(iteration?.evidence || iteration?.evidence_refs)),
    ...normalizeArray(proof.evidence_envelope?.iterations).flatMap((iteration) => normalizeArray(iteration?.evidence || iteration?.evidence_refs)),
  ].filter(isObject);
}

function collectEvents(proof) {
  return [
    ...normalizeArray(proof.events),
    ...normalizeArray(proof.event_log),
    ...normalizeArray(proof.evidence_envelope?.events),
  ].filter(isObject);
}

function collectIterations(proof) {
  return normalizeArray(proof.iterations || proof.loop?.iterations).filter(isObject);
}

function normalizeArtifactDeclarations(value) {
  return normalizeArray(value)
    .map((declaration) => typeof declaration === 'string' ? { id: declaration, required: true } : declaration)
    .filter(isObject)
    .map((declaration) => ({
      ...declaration,
      id: declaration.id || declaration.name || declaration.role,
    }))
    .filter((declaration) => declaration.id);
}

function normalizeEvidenceRequirements(value) {
  return normalizeArray(value)
    .map((requirement) => typeof requirement === 'string' ? { id: requirement } : requirement)
    .filter(isObject);
}

function findArtifact(artifacts, id) {
  return artifacts.find((artifact) => artifact.id === id || artifact.name === id || artifact.role === id || artifact.artifact_id === id) || null;
}

function findEvidence(evidence, requirement) {
  const normalized = typeof requirement === 'string' ? { id: requirement } : optionalObject(requirement);
  return evidence.find((ref) => (
    (normalized.id && (ref.id === normalized.id || ref.name === normalized.id || ref.role === normalized.id || ref.evidence_id === normalized.id))
    || (normalized.kind && ref.kind === normalized.kind)
    || (normalized.type && (ref.type === normalized.type || ref.kind === normalized.type))
    || (normalized.url && refUrl(ref) === normalized.url)
    || (normalized.uri && refUrl(ref) === normalized.uri)
  )) || null;
}

function explicitIterationCount(proof, iterations) {
  const count = positiveInteger(proof.iteration_count || proof.evidence_envelope?.iteration_count || proof.loop?.iteration_count);
  return count || iterations.length;
}

function stopReasonFromProof(proof, iterations) {
  return proof.stop_reason
    || proof.stop?.reason
    || proof.loop?.stop_reason
    || proof.loop?.stop?.reason
    || iterations[iterations.length - 1]?.stop_reason
    || iterations[iterations.length - 1]?.stop?.reason
    || '';
}

function duplicateValues(values) {
  const seen = new Set();
  const duplicates = new Set();
  for (const value of values) {
    if (seen.has(value)) {
      duplicates.add(value);
    }
    seen.add(value);
  }
  return [...duplicates];
}

function requirementLabel(requirement) {
  return requirement.id || requirement.name || requirement.role || requirement.kind || requirement.type || requirement.url || requirement.uri || '(unnamed)';
}

function evidenceLabel(ref) {
  return ref.id || ref.name || ref.role || ref.kind || ref.type || refUrl(ref) || '(unnamed)';
}

function refUrl(ref) {
  return ref.url || ref.uri || ref.href || ref.public_url || ref.publicUrl || ref.artifact_url || ref.artifactUrl || ref.path || '';
}

function durableUrl(url) {
  return typeof url === 'string' && /^https?:\/\//i.test(url) && !localOnlyUrl(url);
}

function localOnlyUrl(url) {
  if (typeof url !== 'string' || url.trim() === '') {
    return false;
  }
  return /^https?:\/\/(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::|\/|$)/i.test(url)
    || /^file:\/\//i.test(url)
    || /^\/Users\//.test(url)
    || /^\/private\//.test(url)
    || /^\/tmp\//.test(url)
    || /^\.\.?\//.test(url);
}

function failure(className, message, data) {
  return { class: className, message, data: data || {} };
}

function positiveInteger(value) {
  const parsed = Number.parseInt(value || '', 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : undefined;
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function optionalObject(value) {
  return isObject(value) ? value : {};
}

function isObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  assertControllerLoopProof,
  validateControllerLoopProof,
};
