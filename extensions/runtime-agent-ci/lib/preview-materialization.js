'use strict';

const PREVIEW_MATERIALIZATION_REQUEST_SCHEMA = 'homeboy/preview-materialization-request/v1';
const PREVIEW_MATERIALIZATION_EVIDENCE_SCHEMA = 'homeboy/preview-materialization-evidence/v1';

async function materializePreview(adapter, request = {}, context = {}) {
  if (!adapter || typeof adapter.materializePreview !== 'function') {
    throw new Error('Preview materialization requires an adapter with materializePreview(request, context).');
  }
  const normalizedRequest = normalizePreviewMaterializationRequest(request);
  const result = await adapter.materializePreview(normalizedRequest, context);
  return normalizePreviewMaterializationEvidence(result, {
    adapter: adapter.id || adapter.provider || normalizedRequest.adapter,
    request: normalizedRequest,
  });
}

function normalizePreviewMaterializationRequest(request = {}) {
  if (!plainObject(request)) {
    throw new Error('Preview materialization request must be an object.');
  }
  const target = firstObject(request.target, request.site, request.source, request.input, request.domainInput, request.domain_input);
  if (!target) {
    throw new Error('Preview materialization request requires target/domain input.');
  }
  return cleanObject({
    schema: request.schema || PREVIEW_MATERIALIZATION_REQUEST_SCHEMA,
    version: request.version || 1,
    id: request.id || request.preview_id || request.previewId,
    adapter: request.adapter || request.provider || request.runtime,
    target,
    routes: firstObject(request.routes),
    open: firstObject(request.open, request.open_options, request.openOptions),
    lease: firstObject(request.lease, request.lease_request, request.leaseRequest),
    boot: firstObject(request.boot, request.boot_config, request.bootConfig),
    metadata: firstObject(request.metadata),
  });
}

function normalizePreviewMaterializationEvidence(value = {}, context = {}) {
  if (!plainObject(value)) {
    throw new Error('Preview materialization evidence must be an object.');
  }
  const url = firstValue(
    value.url,
    value.preview_url,
    value.previewUrl,
    value.open_url,
    value.openUrl,
    value.public_url,
    value.publicUrl,
    value.href,
    value.ref?.url,
    value.ref?.uri,
  );
  if (typeof url !== 'string' || url.trim() === '') {
    throw new Error('Preview materialization evidence requires a URL.');
  }
  return cleanObject({
    schema: value.schema || PREVIEW_MATERIALIZATION_EVIDENCE_SCHEMA,
    version: value.version || 1,
    adapter: value.adapter || value.provider || context.adapter,
    id: value.id || value.preview_id || value.previewId || value.lease?.id || value.lease?.lease_id,
    url,
    refs: normalizePreviewRefs(value.refs || value.evidence_refs || value.evidenceRefs || value.ref || { kind: 'preview', uri: url, label: 'Preview' }),
    lease: firstObject(value.lease, value.preview_lease, value.previewLease),
    boot: firstObject(value.boot, value.boot_config, value.bootConfig),
    status: firstObject(value.status, value.site_status, value.siteStatus),
    metadata: firstObject(value.metadata),
    source_request: value.source_request || value.sourceRequest || context.request,
  });
}

function normalizePreviewRefs(value) {
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
      metadata: firstObject(ref.metadata),
    });
  }).filter((ref) => ref && ref.uri);
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '');
}

function firstObject(...values) {
  return values.find(plainObject);
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
  PREVIEW_MATERIALIZATION_EVIDENCE_SCHEMA,
  PREVIEW_MATERIALIZATION_REQUEST_SCHEMA,
  materializePreview,
  normalizePreviewMaterializationEvidence,
  normalizePreviewMaterializationRequest,
  normalizePreviewRefs,
};
