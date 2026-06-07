#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { URL } from 'node:url';

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const provider = options.provider || process.env.HOMEBOY_PREVIEW_BACKEND_PROVIDER || 'external-broker';
  const localUrl = parseUrl(options.localUrl || process.env.HOMEBOY_SERVICE_LOCAL_URL, 'local URL');
  const publicUrl = parseUrl(options.publicUrl || process.env.HOMEBOY_TUNNEL_PUBLIC_URL, 'public URL');
  const brokerUrlValue = options.brokerUrl || process.env.HOMEBOY_PREVIEW_BROKER_URL || '';
  const expectedEffectiveOrigin = options.expectedEffectiveOrigin || process.env.HOMEBOY_EXPECTED_EFFECTIVE_ORIGIN || '';
  const expectedConfigHostname = options.expectedConfigHostname || process.env.HOMEBOY_EXPECTED_CONFIG_HOSTNAME || '';
  const requireHostPreservation = Boolean(options.requireHostPreservation || expectedEffectiveOrigin || expectedConfigHostname);

  const brokerUrl = brokerUrlValue ? parseUrl(brokerUrlValue, 'broker URL') : null;
  const plan = buildPlan({ provider, localUrl, publicUrl, brokerUrl, options });
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

  const registration = await registerPreview({
    dryRun: options.dryRun,
    brokerUrl,
    provider,
    localUrl,
    publicUrl,
    preservation,
  });

  const startEvidence = {
    schema: 'homeboy/managed-preview-backend-start/v1',
    status: options.dryRun ? 'planned' : 'starting',
    provider,
    local_url: localUrl.href,
    public_url: publicUrl.href,
    command: plan.command,
    args: plan.args,
    host_preservation: preservation,
    registration,
  };
  process.stdout.write(`${JSON.stringify(startEvidence, null, 2)}\n`);

  if (options.dryRun) {
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

  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.on(signal, () => {
      child.kill(signal);
    });
  }

  child.on('exit', (code, signal) => {
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exitCode = code ?? 1;
  });
}

function buildPlan({ provider, localUrl, publicUrl, brokerUrl, options }) {
  if (provider !== 'external-broker') {
    throw new Error(`Unsupported preview backend provider: ${provider}`);
  }

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

async function registerPreview({ dryRun, brokerUrl, provider, localUrl, publicUrl, preservation }) {
  const request = {
    schema: 'homeboy/managed-preview-broker-request/v1',
    provider,
    local_url: localUrl.href,
    public_url: publicUrl.href,
    host_preservation: preservation,
  };

  if (dryRun) {
    return {
      status: 'planned',
      broker_url: brokerUrl?.href || null,
      request,
    };
  }

  const response = await fetch(brokerUrl.href, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(request),
  });

  const text = await response.text();
  const payload = text ? JSON.parse(text) : {};
  if (!response.ok) {
    throw new Error(`Preview broker rejected request with HTTP ${response.status}: ${text}`);
  }
  if (preservation.required && payload?.capabilities?.hostname_preserving_browser_origin !== true) {
    throw new Error('Preview broker response did not prove hostname-preserving browser-origin capability');
  }
  return payload;
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
    options[key] = value;
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
