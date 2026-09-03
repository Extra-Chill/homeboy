'use strict';

const {
  normalizePreviewMaterializationEvidence,
  normalizePreviewMaterializationRequest,
} = require('../../runtime-agent-ci/lib/preview-materialization');
const {
  runtimeContractSchemas,
} = require('./wp-codebox-runtime-contract-source');

const RUNTIME_CONTRACT_SCHEMAS = runtimeContractSchemas();
const WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.browserContainedSiteOpen;
const WP_CODEBOX_BROWSER_CONTAINED_SITE_STATUS_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.browserContainedSiteStatus;
const WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.browserPreviewBootConfig;
const WP_CODEBOX_PREVIEW_LEASE_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.runtimeBoundary.previewLease;

function codeboxPreviewMaterializationAdapter(options = {}) {
  const openContainedSite = options.openContainedSite || options.openPreview || options.materialize;
  if (typeof openContainedSite !== 'function') {
    throw new Error('WP Codebox preview adapter requires openContainedSite(request, context).');
  }
  return {
    id: 'wp-codebox',
    provider: 'wp-codebox',
    async materializePreview(request, context = {}) {
      const genericRequest = normalizePreviewMaterializationRequest(request);
      const codeboxRequest = codeboxPreviewOpenRequest(genericRequest, options);
      const result = await openContainedSite(codeboxRequest, { ...context, genericRequest });
      return codeboxPreviewEvidenceFromContainedSiteResult(result, { request: genericRequest, codeboxRequest });
    },
  };
}

function codeboxPreviewOpenRequest(request = {}, options = {}) {
  const genericRequest = normalizePreviewMaterializationRequest(request);
  return cleanObject({
    schema: WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA,
    version: 1,
    preview_id: genericRequest.id,
    target: genericRequest.target,
    routes: genericRequest.routes,
    open: genericRequest.open,
    lease: codeboxPreviewLeaseRequest(genericRequest.lease),
    boot: codeboxPreviewBootConfig(genericRequest.boot),
    metadata: {
      ...(genericRequest.metadata || {}),
      ...(options.metadata || {}),
      homeboy_preview_request_schema: genericRequest.schema,
    },
  });
}

function codeboxPreviewEvidenceFromContainedSiteResult(result = {}, context = {}) {
  const url = firstValue(
    result.url,
    result.preview_url,
    result.previewUrl,
    result.open_url,
    result.openUrl,
    result.contained_site?.url,
    result.containedSite?.url,
    result.site?.url,
  );
  const lease = firstObject(result.lease, result.preview_lease, result.previewLease, result.contained_site?.lease, result.containedSite?.lease);
  const boot = firstObject(result.boot, result.boot_config, result.bootConfig, result.contained_site?.boot, result.containedSite?.boot);
  const status = firstObject(result.status, result.site_status, result.siteStatus, result.contained_site?.status, result.containedSite?.status);
  return normalizePreviewMaterializationEvidence({
    adapter: 'wp-codebox',
    id: result.id || result.preview_id || result.previewId || lease?.id || lease?.lease_id,
    url,
    refs: result.refs || result.evidence_refs || result.evidenceRefs || { kind: 'codebox-preview', uri: url, label: 'WP Codebox preview' },
    lease,
    boot,
    status,
    metadata: cleanObject({
      codebox_open_schema: WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA,
      codebox_status_schema: status ? WP_CODEBOX_BROWSER_CONTAINED_SITE_STATUS_SCHEMA : undefined,
      codebox_request: context.codeboxRequest,
      ...(result.metadata || {}),
    }),
    source_request: context.request,
  });
}

function codeboxPreviewLeaseRequest(lease) {
  if (!lease) {
    return undefined;
  }
  return cleanObject({
    schema: lease.schema || WP_CODEBOX_PREVIEW_LEASE_SCHEMA,
    ...lease,
  });
}

function codeboxPreviewBootConfig(boot) {
  if (!boot) {
    return undefined;
  }
  return cleanObject({
    schema: boot.schema || WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA,
    ...boot,
  });
}

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '');
}

function firstObject(...values) {
  return values.find((value) => value && typeof value === 'object' && !Array.isArray(value));
}

function cleanObject(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
  WP_CODEBOX_BROWSER_CONTAINED_SITE_OPEN_SCHEMA,
  WP_CODEBOX_BROWSER_CONTAINED_SITE_STATUS_SCHEMA,
  WP_CODEBOX_BROWSER_PREVIEW_BOOT_CONFIG_SCHEMA,
  WP_CODEBOX_PREVIEW_LEASE_SCHEMA,
  codeboxPreviewEvidenceFromContainedSiteResult,
  codeboxPreviewMaterializationAdapter,
  codeboxPreviewOpenRequest,
};
