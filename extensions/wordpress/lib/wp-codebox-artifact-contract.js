'use strict';

const TYPED_ARTIFACT_SCHEMA = 'homeboy/agent-task-typed-artifact/v1';
const {
	runtimeContractSchemas,
} = require('./wp-codebox-runtime-contract-source');
const RUNTIME_CONTRACT_SCHEMAS = runtimeContractSchemas();

const WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA = 'wp-codebox/artifact-declaration/v1';
const WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.artifact.resultEnvelope;
const WP_CODEBOX_CASE_ARTIFACT_INDEX_SCHEMA = 'wp-codebox/case-artifact-index/v1';
const WP_CODEBOX_RUNTIME_ACCESS_SCHEMA = 'wp-codebox/runtime-access/v1';

function normalizeTypedArtifactEntry(name, artifact, options = {}) {
  if (!plainObject(artifact)) {
    return null;
  }
  const coreNormalizer = firstFunction(options.normalizeTypedArtifactEntry, options.normalizeTypedArtifactDto, options.normalizeTypedArtifactDTO);
  if (coreNormalizer) {
    const normalized = normalizeWithCoreTypedArtifactNormalizer(coreNormalizer, name, artifact, options);
    if (plainObject(normalized)) {
      return typeof options.sanitize === 'function' ? options.sanitize(normalized) : normalized;
    }
  }
  const artifactName = artifact.name || name;
  if (!artifactName) {
    return null;
  }
  const entry = cleanObject({
    schema: TYPED_ARTIFACT_SCHEMA,
    name: artifactName,
    type: artifact.type || artifact.kind || artifact.artifact_type,
    artifact_schema: artifact.artifact_schema || artifact.schema,
    payload: artifact.payload !== undefined ? artifact.payload : artifact.data,
    provenance: plainObject(artifact.provenance) ? artifact.provenance : {},
    file_refs: typedArtifactFileRefs(artifact),
    metadata: plainObject(artifact.metadata) ? artifact.metadata : {},
  });
  return typeof options.sanitize === 'function' ? options.sanitize(entry) : entry;
}

function normalizeTypedArtifacts(value, options = {}) {
  const coreNormalizer = firstFunction(options.normalizeTypedArtifacts, options.normalizeTypedArtifactMap);
  if (coreNormalizer) {
    const normalized = normalizeWithCoreTypedArtifactsNormalizer(coreNormalizer, value, options);
    if (plainObject(normalized)) {
      return typeof options.sanitize === 'function' ? options.sanitize(normalized) : normalized;
    }
  }
  if (Array.isArray(value)) {
    return Object.fromEntries(value
      .map((artifact, index) => normalizeTypedArtifactEntry(artifact?.name || artifact?.id || `artifact_${index + 1}`, artifact, options))
      .filter(Boolean)
      .map((artifact) => [artifact.name, artifact]));
  }
  if (!plainObject(value)) {
    return {};
  }
  return Object.fromEntries(Object.entries(value)
    .map(([name, artifact]) => normalizeTypedArtifactEntry(name, artifact, options))
    .filter(Boolean)
    .map((artifact) => [artifact.name, artifact]));
}

function typedArtifactsFromCodeboxResult(result, options = {}) {
  const artifactResult = artifactResultEnvelopeFromCodeboxResult(result, options);
  const typedArtifacts = typedArtifactsFromArtifactResultEnvelope(artifactResult, options);
  const runtimeAccess = runtimeAccessTypedArtifactFromCodeboxResult(result, artifactResult, options);
  if (runtimeAccess && !typedArtifacts.runtime_access) {
    typedArtifacts.runtime_access = runtimeAccess;
  }
  return typedArtifacts;
}

function normalizeWithCoreTypedArtifactNormalizer(normalizer, name, artifact, options = {}) {
  try {
    return normalizer(name, artifact, { sanitize: options.sanitize });
  } catch {
    try {
      return normalizer({ ...artifact, name: artifact.name || name }, { sanitize: options.sanitize });
    } catch {
      return null;
    }
  }
}

function normalizeWithCoreTypedArtifactsNormalizer(normalizer, value, options = {}) {
  try {
    return normalizer(value, { sanitize: options.sanitize });
  } catch {
    return null;
  }
}

function firstFunction(...values) {
  return values.find((value) => typeof value === 'function') || null;
}

function caseArtifactIndexFromCodeboxResult(result, options = {}) {
  const artifactResult = artifactResultEnvelopeFromCodeboxResult(result, options);
  return normalizeCaseArtifactIndex({
    schema: WP_CODEBOX_CASE_ARTIFACT_INDEX_SCHEMA,
    caseRefs: [
      ...caseRefsFromCaseArtifactCandidates(artifactResult),
      ...caseRefsFromCaseArtifactCandidates(artifactResult?.result),
      ...caseRefsFromCaseArtifactCandidates(artifactResult?.result?.outputs),
    ],
    metadata: plainObject(options.metadata) ? options.metadata : undefined,
  });
}

function normalizeCaseArtifactIndex(value = {}) {
  if (!plainObject(value)) {
    return {
      schema: WP_CODEBOX_CASE_ARTIFACT_INDEX_SCHEMA,
      caseRefs: [],
    };
  }
  const candidateRefs = [
    ...(Array.isArray(value.caseRefs) ? value.caseRefs : []),
    ...(Array.isArray(value.case_refs) ? value.case_refs : []),
    ...caseRefsFromFuzzResults(value),
  ];
  return cleanObject({
    schema: WP_CODEBOX_CASE_ARTIFACT_INDEX_SCHEMA,
    caseRefs: dedupeCaseArtifactRefs(candidateRefs.map(normalizeCaseArtifactRef).filter(Boolean)),
    metadata: plainObject(value.metadata) ? value.metadata : undefined,
  });
}

function caseRefsFromCaseArtifactCandidates(value) {
  if (!plainObject(value)) {
    return [];
  }
  const directIndex = value.schema === WP_CODEBOX_CASE_ARTIFACT_INDEX_SCHEMA ? normalizeCaseArtifactIndex(value).caseRefs : [];
  return [
    ...directIndex,
    ...caseRefsFromFuzzResults(value),
    ...caseRefsFromFuzzResults(value.fuzz_results),
    ...caseRefsFromFuzzResults(value.fuzzResults),
    ...caseRefsFromFuzzResults(value.benchmark_artifacts),
    ...caseRefsFromFuzzResults(value.benchmarkArtifacts),
    ...caseRefsFromFuzzResults(value.artifacts),
  ];
}

function caseRefsFromFuzzResults(value) {
  if (Array.isArray(value)) {
    return value.flatMap((entry, index) => caseRefsFromScenario(entry, { caseIndex: index }));
  }
  if (!plainObject(value)) {
    return [];
  }
  if (Array.isArray(value.results)) {
    return value.results.flatMap((result) => caseRefsFromBenchmarkResult(result));
  }
  if (Array.isArray(value.scenarios)) {
    return value.scenarios.flatMap((scenario) => caseRefsFromScenario(scenario, {
      componentId: value.component_id || value.componentId,
    }));
  }
  if (Array.isArray(value.cases) || Array.isArray(value.steps)) {
    return caseRefsFromScenario(value, {
      componentId: value.component_id || value.componentId,
      scenarioId: value.scenario_id || value.scenarioId || value.id,
    });
  }
  return [];
}

function caseRefsFromBenchmarkResult(result) {
  if (!plainObject(result)) {
    return [];
  }
  const componentId = result.component_id || result.componentId;
  return normalizeArray(result.scenarios).flatMap((scenario) => caseRefsFromScenario(scenario, { componentId }));
}

function caseRefsFromScenario(scenario, context = {}) {
  if (!plainObject(scenario)) {
    return [];
  }
  const componentId = scenario.component_id || scenario.componentId || context.componentId;
  const scenarioId = scenario.scenario_id || scenario.scenarioId || scenario.id || context.scenarioId;
  const scenarioArtifactRefs = artifactRefsFromValue(scenario.artifactRefs || scenario.artifact_refs || scenario.artifacts);
  const caseCandidates = [
    ...normalizeArray(scenario.cases),
    ...caseCandidatesFromSteps(scenario.steps),
  ];
  if (caseCandidates.length === 0 && (scenario.case_id || scenario.caseId || scenario.name || scenarioArtifactRefs.length > 0)) {
    caseCandidates.push(scenario);
  }
  return caseCandidates.map((candidate, index) => normalizeCaseArtifactRef({
    component_id: componentId,
    scenario_id: scenarioId,
    case_id: candidate?.case_id || candidate?.caseId || candidate?.id || candidate?.name || context.caseId,
    index: candidate?.index ?? candidate?.case_index ?? candidate?.caseIndex ?? context.caseIndex ?? index,
    status: candidate?.status,
    artifactRefs: [
      ...scenarioArtifactRefs,
      ...artifactRefsFromValue(candidate?.artifactRefs || candidate?.artifact_refs || candidate?.artifacts),
    ],
    metadata: plainObject(candidate?.metadata) ? candidate.metadata : undefined,
  })).filter((caseRef) => caseRef && (caseRef.case_id || caseRef.artifactRefs.length > 0));
}

function caseCandidatesFromSteps(steps) {
  return normalizeArray(steps)
    .filter((step) => plainObject(step) && (step.case_id || step.caseId || typeof step.rest_request_case_index === 'number' || typeof step.case_index === 'number'))
    .map((step) => ({
      ...step,
      case_id: step.case_id || step.caseId,
      index: step.rest_request_case_index ?? step.case_index ?? step.caseIndex,
    }));
}

function normalizeCaseArtifactRef(ref) {
  if (!plainObject(ref)) {
    return null;
  }
  const artifactRefs = artifactRefsFromValue(ref.artifactRefs || ref.artifact_refs);
  return cleanObject({
    component_id: ref.component_id || ref.componentId,
    scenario_id: ref.scenario_id || ref.scenarioId,
    case_id: ref.case_id || ref.caseId || ref.id,
    index: typeof ref.index === 'number' ? ref.index : undefined,
    status: ref.status,
    artifactRefs: dedupeArtifactRefs(artifactRefs),
    metadata: plainObject(ref.metadata) ? ref.metadata : undefined,
  });
}

function artifactRefsFromValue(value) {
  if (Array.isArray(value)) {
    return value.flatMap(artifactRefsFromValue);
  }
  if (typeof value === 'string') {
    return [normalizeCaseArtifactFileRef({ path: value })];
  }
  if (!plainObject(value)) {
    return [];
  }
  if (typeof value.path === 'string' || typeof value.uri === 'string' || typeof value.url === 'string') {
    return [normalizeCaseArtifactFileRef(value)].filter(Boolean);
  }
  return Object.entries(value).flatMap(([name, artifact]) => artifactRefsFromValueWithName(name, artifact));
}

function artifactRefsFromValueWithName(name, value) {
  return artifactRefsFromValue(value).map((ref) => cleanObject({ name, ...ref }));
}

function normalizeCaseArtifactFileRef(ref) {
  if (!plainObject(ref)) {
    return null;
  }
  const digest = plainObject(ref.digest) ? ref.digest : {};
  return cleanObject({
    path: ref.path || ref.uri,
    uri: ref.uri,
    url: ref.url,
    kind: ref.kind || ref.type,
    contentType: ref.contentType || ref.content_type || ref.mime,
    sha256: ref.sha256 || digest.value,
    source: ref.source,
    name: ref.name,
    metric: ref.metric,
    sampleIndex: ref.sampleIndex ?? ref.sample_index,
  });
}

function dedupeCaseArtifactRefs(refs) {
  const seen = new Set();
  return refs.filter((ref) => {
    const key = `${ref.component_id || ''}:${ref.scenario_id || ''}:${ref.case_id || ''}:${ref.index ?? ''}:${JSON.stringify(ref.artifactRefs || [])}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function dedupeArtifactRefs(refs) {
  const seen = new Set();
  return refs.filter((ref) => {
    const key = `${ref.path || ''}:${ref.uri || ''}:${ref.url || ''}:${ref.kind || ''}:${ref.name || ''}:${ref.sha256 || ''}:${ref.sampleIndex ?? ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function typedArtifactsFromArtifactResultEnvelope(artifactResult, options = {}) {
  const candidates = [
    artifactResult?.typed_artifacts,
    artifactResult?.typedArtifacts,
    artifactResult?.result?.typed_artifacts,
    artifactResult?.result?.typedArtifacts,
    artifactResult?.result?.outputs?.typed_artifacts,
    artifactResult?.result?.outputs?.typedArtifacts,
  ];
  return Object.assign({}, ...candidates.map((candidate) => normalizeTypedArtifacts(candidate, options)));
}

function artifactResultEnvelopeFromCodeboxResult(result, options = {}) {
  const candidates = [
    result,
    result?.artifact_result,
    result?.outputs?.artifact_result,
  ];
  return candidates.map(normalizeArtifactResultEnvelope).find(Boolean) || null;
}

function normalizeCodeboxPublicResultEnvelope(result, options = {}) {
  const artifactResult = artifactResultEnvelopeFromCodeboxResult(result, options);
  if (!artifactResult) {
    return null;
  }
  const payload = plainObject(artifactResult.result) ? artifactResult.result : {};
  const outputs = plainObject(payload.outputs) ? payload.outputs : {};
  const metadata = plainObject(artifactResult.metadata) ? artifactResult.metadata : {};
  return cleanObject({
    schema: 'wp-codebox/public-result-envelope/v1',
    artifact_result: artifactResult,
    status: payload.status || artifactResult.status,
    success: payload.success === undefined ? artifactResult.success : payload.success === true,
    summary: payload.summary || payload.message || artifactResult.reason,
    message: payload.message,
    error_message: payload.error_message || payload.errorMessage,
    error_reason: payload.error_reason || payload.errorReason,
    error_step_id: payload.error_step_id || payload.errorStepId,
    terminal_status: payload.terminal_status || payload.terminalStatus,
    completion_outcome: payload.completion_outcome || payload.completionOutcome,
    reply: firstString(payload.reply, payload.text, outputs.reply, outputs.text, outputs.content),
    outputs,
    artifacts: artifactResult.artifactRefs || [],
    evidence_refs: artifactResult.evidenceRefs || [],
    diagnostics: [
      ...(Array.isArray(payload.diagnostics) ? payload.diagnostics : []),
      ...(Array.isArray(artifactResult.diagnostics) ? artifactResult.diagnostics : []),
    ],
    metadata,
  });
}

function firstString(...values) {
	return values.find((value) => typeof value === 'string' && value.trim() !== '');
}

function firstObject(...values) {
  return values.find(plainObject);
}

function normalizeArtifactResultEnvelope(envelope) {
  if (!plainObject(envelope) || envelope.schema !== WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA) {
    return null;
  }
  const artifactBundle = normalizeArtifactResultRef(envelope.artifactBundle || envelope.artifact_bundle);
  const artifactRefs = [
    artifactBundle,
    ...(Array.isArray(envelope.artifactBundleRefs) ? envelope.artifactBundleRefs.map(normalizeArtifactResultRef) : []),
    ...(Array.isArray(envelope.artifact_bundle_refs) ? envelope.artifact_bundle_refs.map(normalizeArtifactResultRef) : []),
    ...(Array.isArray(envelope.artifactRefs) ? envelope.artifactRefs.map(normalizeArtifactResultRef) : []),
    ...(Array.isArray(envelope.artifact_refs) ? envelope.artifact_refs.map(normalizeArtifactResultRef) : []),
  ].filter(Boolean);
  const evidenceRefs = [
    ...(Array.isArray(envelope.evidenceRefs) ? envelope.evidenceRefs.map(normalizeArtifactResultRef) : []),
    ...(Array.isArray(envelope.evidence_refs) ? envelope.evidence_refs.map(normalizeArtifactResultRef) : []),
    ...(Array.isArray(envelope.transcriptRefs) ? envelope.transcriptRefs.map(normalizeArtifactResultRef) : []),
    ...(Array.isArray(envelope.transcript_refs) ? envelope.transcript_refs.map(normalizeArtifactResultRef) : []),
  ].filter(Boolean);
  return cleanObject({
    schema: WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
    operation: envelope.operation,
    operation_schema: envelope.operation_schema,
    status: envelope.status,
    success: envelope.success === undefined ? ['created', 'existing', 'updated'].includes(envelope.status) : envelope.success === true,
    artifactBundle,
    artifactRefs: uniqueArtifactResultRefs(artifactRefs),
    evidenceRefs: uniqueArtifactResultRefs(evidenceRefs),
    structured_artifacts: Array.isArray(envelope.structured_artifacts) ? envelope.structured_artifacts : envelope.structuredArtifacts,
    typed_artifacts: Array.isArray(envelope.typed_artifacts) ? envelope.typed_artifacts : envelope.typedArtifacts,
    verification: plainObject(envelope.verification) ? envelope.verification : undefined,
    result: plainObject(envelope.result) ? envelope.result : undefined,
    diagnostics: Array.isArray(envelope.diagnostics) ? envelope.diagnostics : [],
    metadata: plainObject(envelope.metadata) ? envelope.metadata : undefined,
    error: plainObject(envelope.error) ? envelope.error : undefined,
    reason: envelope.reason,
  });
}

function normalizeArtifactResultRef(ref) {
  if (!plainObject(ref)) {
    return null;
  }
  const digest = plainObject(ref.digest) ? ref.digest : {};
  return cleanObject({
    id: ref.id || ref.artifact_id,
    kind: ref.kind || ref.type || 'artifact-bundle',
    name: ref.name || ref.label,
    path: ref.path || ref.artifacts_path || ref.directory || ref.uri,
    uri: ref.uri,
    url: ref.url,
    sha256: ref.sha256 || digest.value,
    metadata: cleanObject({
      digest: plainObject(ref.digest) ? ref.digest : undefined,
      source_schema: WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
    }),
  });
}

function uniqueArtifactResultRefs(refs) {
  const seen = new Set();
  return refs.filter((ref) => {
    const key = `${ref.kind || ''}:${ref.id || ''}:${ref.path || ''}:${ref.url || ''}:${ref.sha256 || ''}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

function normalizeCodeboxArtifactDeclaration(defaultName, declaration, options = {}) {
  if (typeof declaration === 'string') {
    return {
      schema: WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
      name: declaration,
      required: true,
    };
  }
  if (!plainObject(declaration)) {
    return null;
  }
  const name = declaration.name || declaration.id || declaration.output || declaration.artifact || defaultName;
  if (!name || typeof name !== 'string') {
    return null;
  }
  const ignoredSchemas = new Set([
    WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
    ...normalizeArray(options.ignoredSchemas),
  ]);
  const artifactSchema = declaration.artifact_schema
    || declaration.content_schema
    || (declaration.schema && !ignoredSchemas.has(declaration.schema) ? declaration.schema : undefined);
  return cleanObject({
    schema: WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
    name,
    type: declaration.type || declaration.kind || declaration.artifact_type,
    artifact_schema: artifactSchema,
    path: declaration.path,
    required: declaration.required === undefined ? true : declaration.required === true,
    description: declaration.description,
    metadata: declaration.metadata,
  });
}

function normalizeCodeboxArtifactOutcome(artifact, rawArtifact = {}, options = {}) {
  if (!artifact) {
    return artifact;
  }
  const nativeKind = rawArtifact.kind || rawArtifact.type || artifact.kind || '';
  const role = artifactRoleFromCodeboxArtifact({ ...artifact, kind: nativeKind }, options.roleAliases || {});
  const metadata = plainObject(artifact.metadata) ? artifact.metadata : {};
  const sanitize = typeof options.sanitize === 'function' ? options.sanitize : (value) => value;
  return {
    ...artifact,
    role,
    metadata: sanitize({
      ...metadata,
      wp_codebox: {
        id: rawArtifact.id || artifact.id,
        kind: nativeKind,
        name: rawArtifact.name || artifact.name,
        raw: rawArtifact,
      },
    }),
  };
}

function typedArtifactFileRefs(artifact) {
  if (Array.isArray(artifact?.file_refs)) {
    return artifact.file_refs;
  }
  return [];
}

function runtimeAccessTypedArtifactFromCodeboxResult(result, artifactResult, options = {}) {
  const payload = runtimeAccessPayloadFromCodeboxResult(result, artifactResult);
  if (!payload) {
    return null;
  }
  return normalizeTypedArtifactEntry('runtime_access', {
    type: 'json',
    artifact_schema: WP_CODEBOX_RUNTIME_ACCESS_SCHEMA,
    payload,
    provenance: {
      source: 'wp-codebox-runtime-output',
    },
  }, options);
}

function runtimeAccessPayloadFromCodeboxResult(result, artifactResult) {
  const evidence = runtimeAccessEvidenceCandidates(result, artifactResult)
    .map(normalizeRuntimeAccessEvidence)
    .find(Boolean);
  if (!evidence) {
    return null;
  }
  return cleanObject({
    schema: WP_CODEBOX_RUNTIME_ACCESS_SCHEMA,
    preview_url: evidence.preview_url,
    public_url: evidence.public_url,
    reviewer_url: evidence.reviewer_url,
    site_url: evidence.site_url,
    admin_url: evidence.admin_url,
    lease: evidence.lease,
    reviewer: evidence.reviewer,
    status: evidence.status,
    refs: evidence.refs,
  });
}

function runtimeAccessEvidenceCandidates(result, artifactResult) {
  const payload = plainObject(artifactResult?.result) ? artifactResult.result : {};
  const outputs = plainObject(payload.outputs) ? payload.outputs : {};
  const metadata = plainObject(artifactResult?.metadata) ? artifactResult.metadata : {};
  return [
    outputs.runtime_access,
    outputs.runtimeAccess,
    outputs.preview_materialization,
    outputs.previewMaterialization,
    outputs.preview_evidence,
    outputs.previewEvidence,
    outputs.preview_access,
    outputs.previewAccess,
    payload.runtime_access,
    payload.runtimeAccess,
    payload.preview_materialization,
    payload.previewMaterialization,
    payload.preview_access,
    payload.previewAccess,
    metadata.runtime_access,
    metadata.runtimeAccess,
    metadata.preview_materialization,
    metadata.previewMaterialization,
    metadata.preview_access,
    metadata.previewAccess,
    result?.preview_access,
    result?.previewAccess,
    result?.runtime_access,
    result?.runtimeAccess,
    result?.preview_materialization,
    result?.previewMaterialization,
  ].filter(plainObject);
}

function normalizeRuntimeAccessEvidence(value) {
  const site = firstObject(value.site, value.contained_site, value.containedSite);
  const urls = firstObject(value.urls, value.access_urls, value.accessUrls);
  const previewUrl = firstString(
    value.preview_url,
    value.previewUrl,
    urls?.preview_url,
    urls?.previewUrl,
    value.reviewer_url,
    value.reviewerUrl,
    urls?.reviewer_url,
    urls?.reviewerUrl,
    value.url,
    value.open_url,
    value.openUrl,
  );
  const publicUrl = firstString(value.public_url, value.publicUrl, urls?.public_url, urls?.publicUrl);
  const siteUrl = firstString(value.site_url, value.siteUrl, urls?.site_url, urls?.siteUrl, site?.url, site?.site_url, site?.siteUrl);
  const adminUrl = firstString(value.admin_url, value.adminUrl, urls?.admin_url, urls?.adminUrl, site?.admin_url, site?.adminUrl);
  const refs = normalizeRuntimeAccessRefs(value.refs || value.evidence_refs || value.evidenceRefs || value.ref);
  if (![previewUrl, publicUrl, siteUrl, adminUrl].some(Boolean) && refs.length === 0) {
    return null;
  }
  return cleanObject({
    preview_url: previewUrl,
    public_url: publicUrl,
    reviewer_url: firstString(value.reviewer_url, value.reviewerUrl, urls?.reviewer_url, urls?.reviewerUrl),
    site_url: siteUrl,
    admin_url: adminUrl,
    lease: firstObject(value.lease, value.preview_lease, value.previewLease, site?.lease),
    reviewer: firstObject(value.reviewer, value.review, value.viewer),
    status: firstObject(value.status, value.site_status, value.siteStatus, site?.status),
    refs,
  });
}

function normalizeRuntimeAccessRefs(value) {
  const refs = Array.isArray(value) ? value : [value];
  return refs.map((ref) => {
    if (typeof ref === 'string') {
      return { kind: 'preview', uri: ref, label: 'Preview' };
    }
    if (!plainObject(ref)) {
      return null;
    }
    return cleanObject({
      kind: ref.kind || ref.type || 'preview',
      uri: ref.uri || ref.url || ref.href,
      url: ref.url,
      label: ref.label || ref.name || 'Preview',
    });
  }).filter((ref) => ref && ref.uri);
}

function artifactNameFromDeclaration(declaration) {
  if (typeof declaration === 'string') {
    return declaration;
  }
  if (!plainObject(declaration)) {
    return '';
  }
  return declaration.name || declaration.id || '';
}

function artifactPath(root, relativePath) {
  if (!root || !relativePath) {
    return '';
  }
  return `${String(root).replace(/\/$/, '')}/${String(relativePath).replace(/^\//, '')}`;
}

function artifactRoleFromCodeboxArtifact(artifact = {}, roleAliases = {}) {
  const labels = [artifact.role, artifact.kind, artifact.type].filter(Boolean).map(String);
  const explicitRole = roleFromAliases(labels, roleAliases);
  return explicitRole || artifact.role || artifact.kind || artifact.type || 'artifact';
}

function roleFromAliases(labels, roleAliases = {}) {
  const normalizedLabels = labels.map(normalizeLabel).filter(Boolean);
  const aliasGroups = [
    roleAliases.artifact_roles,
    roleAliases.artifact_kinds,
  ].filter(plainObject);
  for (const aliases of aliasGroups) {
    for (const [role, values] of Object.entries(aliases)) {
      if (normalizeArray(values).map(normalizeLabel).some((alias) => normalizedLabels.includes(alias))) {
        return role;
      }
    }
  }
  return '';
}

function normalizeLabel(value) {
  return String(value || '').trim().toLowerCase();
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function normalizeArray(value) {
  return Array.isArray(value) ? value : [];
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value)
    .filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0)));
}

module.exports = {
  TYPED_ARTIFACT_SCHEMA,
  WP_CODEBOX_ARTIFACT_DECLARATION_SCHEMA,
  WP_CODEBOX_ARTIFACT_RESULT_ENVELOPE_SCHEMA,
  WP_CODEBOX_CASE_ARTIFACT_INDEX_SCHEMA,
  WP_CODEBOX_RUNTIME_ACCESS_SCHEMA,
  artifactResultEnvelopeFromCodeboxResult,
  artifactRoleFromCodeboxArtifact,
  artifactNameFromDeclaration,
  artifactPath,
  caseArtifactIndexFromCodeboxResult,
  normalizeCodeboxArtifactDeclaration,
  normalizeCodeboxArtifactOutcome,
  normalizeCodeboxPublicResultEnvelope,
  normalizeArtifactResultEnvelope,
  normalizeCaseArtifactIndex,
  normalizeTypedArtifactEntry,
  normalizeTypedArtifacts,
  typedArtifactsFromCodeboxResult,
  typedArtifactFileRefs,
};
