#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { URL } from 'node:url';

const PROVIDERS = {
  'external-broker': {
    buildPlan,
    registerPreview,
  },
};

const REGISTER_RETRY_DELAYS_MS = [250, 750];
const REQUEST_TIMEOUT_MS = 5000;
const CLEANUP_TIMEOUT_MS = 3000;

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const provider = options.provider || process.env.HOMEBOY_PREVIEW_BACKEND_PROVIDER || 'external-broker';
  const localUrl = parseUrl(options.localUrl || process.env.HOMEBOY_SERVICE_LOCAL_URL, 'local URL');
  const publicUrl = parseUrl(options.publicUrl || process.env.HOMEBOY_TUNNEL_PUBLIC_URL, 'public URL');
  const brokerUrlValue = options.brokerUrl || process.env.HOMEBOY_PREVIEW_BROKER_URL || '';
  const expectedEffectiveOrigin = options.expectedEffectiveOrigin || process.env.HOMEBOY_EXPECTED_EFFECTIVE_ORIGIN || '';
  const expectedConfigHostname = options.expectedConfigHostname || process.env.HOMEBOY_EXPECTED_CONFIG_HOSTNAME || '';
  const requireHostPreservation = Boolean(options.requireHostPreservation || expectedEffectiveOrigin || expectedConfigHostname);
  const target = buildTargetMetadata(options);

  const brokerUrl = brokerUrlValue ? parseUrl(brokerUrlValue, 'broker URL') : null;
  const strategy = PROVIDERS[provider];
  if (!strategy) {
    throw new Error(`Unsupported preview backend provider: ${provider}`);
  }

  const plan = strategy.buildPlan({ provider, localUrl, publicUrl, brokerUrl, options });
  const preservation = evaluateHostPreservation({
    localUrl,
    publicUrl,
    brokerUrl,
    expectedEffectiveOrigin,
    expectedConfigHostname,
    requireHostPreservation,
    provider,
  });

  if (!preservation.supported) {
    const blocker = {
      schema: 'homeboy/managed-preview-backend-blocker/v1',
      status: 'blocked',
      provider,
      reason: preservation.reason,
      local_url: localUrl.href,
      public_url: publicUrl.href,
      expected_effective_origin: expectedEffectiveOrigin || null,
      expected_config_hostname: expectedConfigHostname || null,
      broker_url: brokerUrl?.href || null,
      recommendation: preservation.recommendation,
    };
    process.stderr.write(`${JSON.stringify(blocker, null, 2)}\n`);
    process.exitCode = 78;
    return;
  }

  const registration = await strategy.registerPreview({
    dryRun: options.dryRun,
    brokerUrl,
    provider,
    localUrl,
    publicUrl,
    preservation,
    target,
  });
  const heartbeat = startHeartbeat(registration);
  const cleanup = createCleanup({ registration, heartbeat });

  const startEvidence = {
    schema: 'homeboy/managed-preview-backend-start/v1',
    status: options.dryRun ? 'planned' : 'starting',
    provider,
    local_url: localUrl.href,
    public_url: publicUrl.href,
    target,
    command: plan.command,
    args: plan.args,
    host_preservation: preservation,
    registration,
  };
  if (options.dryRun) {
    process.stdout.write(`${JSON.stringify(startEvidence, null, 2)}\n`);
    return;
  }

  const child = spawn(plan.command, plan.args, {
    stdio: 'inherit',
    env: {
      ...process.env,
      HOMEBOY_SERVICE_LOCAL_URL: localUrl.href,
      HOMEBOY_TUNNEL_PUBLIC_URL: publicUrl.href,
    },
  });
  let shuttingDown = false;

  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.on(signal, async () => {
      if (shuttingDown) {
        return;
      }
      shuttingDown = true;
      child.kill(signal);
      await waitForChildExit(child, 1000);
      await cleanup();
      process.exit(signalExitCode(signal));
    });
  }

  child.on('exit', async (code, signal) => {
    if (shuttingDown) {
      return;
    }
    shuttingDown = true;
    await cleanup();
    process.exitCode = code ?? 1;
    if (signal) {
      process.exit(signalExitCode(signal));
    }
  });

  process.stdout.write(`${JSON.stringify(startEvidence, null, 2)}\n`);
}

function buildPlan({ provider, localUrl, publicUrl, brokerUrl, options }) {
  return {
    command: options.keepaliveCommand || process.env.HOMEBOY_PREVIEW_KEEPALIVE_COMMAND || 'node',
    args: ['-e', `setInterval(() => {}, 2147483647)`],
    broker_url: brokerUrl?.href || null,
    local_url: localUrl.href,
    public_url: publicUrl.href,
  };
}

function evaluateHostPreservation({
  localUrl,
  publicUrl,
  brokerUrl,
  expectedEffectiveOrigin,
  expectedConfigHostname,
  requireHostPreservation,
  provider,
}) {
  if (!brokerUrl) {
    return {
      required: requireHostPreservation,
      supported: false,
      mode: 'missing-preview-broker',
      reason: 'A hostname-preserving managed preview requires HOMEBOY_PREVIEW_BROKER_URL or --broker-url.',
      recommendation: 'Configure an external preview broker endpoint that can create reviewer-accessible sessions while preserving the browser-effective origin.',
    };
  }

  if (!requireHostPreservation) {
    return {
      required: false,
      supported: true,
      mode: 'broker-public-url-only',
    };
  }

  const expectedOrigin = expectedEffectiveOrigin || localUrl.origin;
  const expectedHostname = expectedConfigHostname || localUrl.hostname;
  return {
    required: true,
    supported: true,
    mode: 'broker-must-prove-host-preservation',
    provider,
    broker_url: brokerUrl.href,
    public_review_url: publicUrl.href,
    expected_effective_origin: expectedOrigin,
    expected_config_hostname: expectedHostname,
  };
}

async function registerPreview({ dryRun, brokerUrl, provider, localUrl, publicUrl, preservation, target }) {
  const request = {
    schema: 'homeboy/managed-preview-broker-request/v1',
    provider,
    local_url: localUrl.href,
    public_url: publicUrl.href,
    target,
    host_preservation: preservation,
  };

  if (dryRun) {
    return {
      status: 'planned',
      broker_url: brokerUrl?.href || null,
      request,
    };
  }

  const response = await fetchWithRetry(brokerUrl.href, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(request),
  });

  const payload = parseJsonResponse(response.text, 'Preview broker registration response');
  if (!response.ok) {
    throw new Error(`Preview broker rejected request with HTTP ${response.status}: ${response.text}`);
  }
  if (preservation.required && payload?.capabilities?.hostname_preserving_browser_origin !== true) {
    throw new Error('Preview broker response did not prove hostname-preserving browser-origin capability');
  }
  return normalizeRegistration(payload, brokerUrl);
}

async function fetchWithRetry(url, init) {
  let lastError = null;
  for (let attempt = 0; attempt <= REGISTER_RETRY_DELAYS_MS.length; attempt += 1) {
    const response = await fetchWithTimeout(url, init, REQUEST_TIMEOUT_MS);
    if (!isTransientResponse(response) || attempt === REGISTER_RETRY_DELAYS_MS.length) {
      return response;
    }
    lastError = new Error(`HTTP ${response.status}`);
    await sleep(REGISTER_RETRY_DELAYS_MS[attempt]);
  }
  throw lastError;
}

async function fetchWithTimeout(url, init, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(url, { ...init, signal: controller.signal });
    return {
      ok: response.ok,
      status: response.status,
      text: await response.text(),
    };
  } catch (error) {
    return {
      ok: false,
      status: 0,
      text: error.name === 'AbortError' ? `Timed out after ${timeoutMs}ms` : error.message,
    };
  } finally {
    clearTimeout(timer);
  }
}

function isTransientResponse(response) {
  return response.status === 0 || response.status === 408 || response.status === 429 || response.status >= 500;
}

function parseJsonResponse(text, label) {
  if (!text) {
    return {};
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`${label} was not valid JSON: ${error.message}`);
  }
}

function normalizeRegistration(payload, brokerUrl) {
  const sessionId = firstString(payload, ['session_id', 'sessionId', 'registration_id', 'registrationId', 'id']);
  const explicitUnregisterUrl = firstString(payload, ['unregister_url', 'unregisterUrl', 'delete_url', 'deleteUrl', 'cleanup_url', 'cleanupUrl']);
  const sessionUrl = firstString(payload, ['session_url', 'sessionUrl', 'url']);
  const unregisterUrlValue = explicitUnregisterUrl || sessionUrl || (sessionId ? new URL(encodeURIComponent(sessionId), ensureTrailingSlash(brokerUrl)).href : null);
  const unregisterUrl = unregisterUrlValue ? resolveUrl(unregisterUrlValue, brokerUrl) : null;
  const heartbeatUrl = firstString(payload, ['heartbeat_url', 'heartbeatUrl']) || firstString(payload?.heartbeat, ['url']);
  const heartbeatIntervalMs = positiveInteger(
    payload?.heartbeat_interval_ms || payload?.heartbeatIntervalMs || payload?.heartbeat?.interval_ms || payload?.heartbeat?.intervalMs
  );

  return {
    ...payload,
    lifecycle: {
      session_id: sessionId,
      unregister_url: unregisterUrl,
      unregister_method: unregisterUrl ? 'DELETE' : null,
      heartbeat: heartbeatUrl ? {
        supported: true,
        url: resolveUrl(heartbeatUrl, brokerUrl),
        interval_ms: heartbeatIntervalMs || 30000,
        method: 'POST',
      } : {
        supported: false,
        reason: 'Broker registration response did not include a heartbeat URL.',
      },
    },
  };
}

function startHeartbeat(registration) {
  const heartbeat = registration?.lifecycle?.heartbeat;
  if (!heartbeat?.supported || !heartbeat.url) {
    return null;
  }
  const timer = setInterval(() => {
    fetchWithTimeout(heartbeat.url, { method: heartbeat.method || 'POST' }, REQUEST_TIMEOUT_MS).catch(() => {});
  }, heartbeat.interval_ms);
  timer.unref?.();
  return timer;
}

function createCleanup({ registration, heartbeat }) {
  let cleanupPromise = null;
  return async () => {
    if (cleanupPromise) {
      return cleanupPromise;
    }
    cleanupPromise = (async () => {
      if (heartbeat) {
        clearInterval(heartbeat);
      }
      const unregisterUrl = registration?.lifecycle?.unregister_url;
      if (!unregisterUrl) {
        return;
      }
      await fetchWithTimeout(unregisterUrl, { method: registration.lifecycle.unregister_method || 'DELETE' }, CLEANUP_TIMEOUT_MS);
    })().catch(() => {});
    return cleanupPromise;
  };
}

function firstString(source, keys) {
  if (!source || typeof source !== 'object') {
    return null;
  }
  for (const key of keys) {
    if (typeof source[key] === 'string' && source[key]) {
      return source[key];
    }
  }
  return null;
}

function positiveInteger(value) {
  const number = Number(value);
  return Number.isInteger(number) && number > 0 ? number : null;
}

function ensureTrailingSlash(url) {
  return url.href.endsWith('/') ? url : new URL(`${url.href}/`);
}

function resolveUrl(value, baseUrl) {
  return new URL(value, baseUrl).href;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, timeoutMs);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

function signalExitCode(signal) {
  return signal === 'SIGINT' ? 130 : 143;
}

function buildTargetMetadata(options) {
  const routes = {};
  for (const route of options.routes || []) {
    const separator = route.indexOf('=');
    if (separator <= 0) {
      throw new Error(`Invalid --route value: ${route}`);
    }
    const name = route.slice(0, separator);
    const value = route.slice(separator + 1);
    routes[name] = parseUrl(value, `route ${name}`).href;
  }

  const target = {
    id: options.targetId || process.env.HOMEBOY_PREVIEW_TARGET_ID || null,
    url: options.targetUrl || process.env.HOMEBOY_PREVIEW_TARGET_URL || null,
    routes,
  };

  if (target.url) {
    target.url = parseUrl(target.url, 'target URL').href;
  }

  if (!target.id && !target.url && Object.keys(routes).length === 0) {
    return null;
  }

  return target;
}

function parseUrl(value, label) {
  if (!value) {
    throw new Error(`Missing ${label}`);
  }
  try {
    return new URL(value);
  } catch (_error) {
    throw new Error(`Invalid ${label}: ${value}`);
  }
}

function parseArgs(args) {
  const options = {};
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === '--dry-run') {
      options.dryRun = true;
      continue;
    }
    if (arg === '--require-host-preservation') {
      options.requireHostPreservation = true;
      continue;
    }
    if (!arg.startsWith('--')) {
      throw new Error(`Unexpected positional argument: ${arg}`);
    }
    const key = toCamelCase(arg.slice(2));
    const value = args[i + 1];
    if (!value || value.startsWith('--')) {
      throw new Error(`Missing value for ${arg}`);
    }
    if (key === 'route') {
      options.routes ||= [];
      options.routes.push(value);
    } else {
      options[key] = value;
    }
    i += 1;
  }
  return options;
}

function toCamelCase(value) {
  return value.replace(/-([a-z])/g, (_, char) => char.toUpperCase());
}

main().catch((error) => {
  process.stderr.write(`${JSON.stringify({ schema: 'homeboy/managed-preview-backend-error/v1', status: 'failed', error: error.message }, null, 2)}\n`);
  process.exitCode = 1;
});
