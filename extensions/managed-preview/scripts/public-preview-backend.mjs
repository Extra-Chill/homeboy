#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { URL } from 'node:url';

const KIMAKI_DEFAULT_DOMAIN = 'kimaki.dev';

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const provider = options.provider || process.env.HOMEBOY_PREVIEW_BACKEND_PROVIDER || 'kimaki';
  const localUrl = parseUrl(options.localUrl || process.env.HOMEBOY_SERVICE_LOCAL_URL, 'local URL');
  const publicUrl = parseUrl(options.publicUrl || process.env.HOMEBOY_TUNNEL_PUBLIC_URL, 'public URL');
  const expectedEffectiveOrigin = options.expectedEffectiveOrigin || process.env.HOMEBOY_EXPECTED_EFFECTIVE_ORIGIN || '';
  const expectedConfigHostname = options.expectedConfigHostname || process.env.HOMEBOY_EXPECTED_CONFIG_HOSTNAME || '';
  const requireHostPreservation = Boolean(options.requireHostPreservation || expectedEffectiveOrigin || expectedConfigHostname);

  const plan = buildPlan({ provider, localUrl, publicUrl, options });
  const preservation = evaluateHostPreservation({
    localUrl,
    publicUrl,
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
      observed_public_origin: publicUrl.origin,
      observed_public_hostname: publicUrl.hostname,
      recommendation: preservation.recommendation,
    };
    process.stderr.write(`${JSON.stringify(blocker, null, 2)}\n`);
    process.exitCode = 78;
    return;
  }

  const startEvidence = {
    schema: 'homeboy/managed-preview-backend-start/v1',
    status: options.dryRun ? 'planned' : 'starting',
    provider,
    local_url: localUrl.href,
    public_url: publicUrl.href,
    command: plan.command,
    args: plan.args,
    host_preservation: preservation,
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

function buildPlan({ provider, localUrl, publicUrl, options }) {
  if (provider !== 'kimaki') {
    throw new Error(`Unsupported preview backend provider: ${provider}`);
  }

  const tunnelId = options.tunnelId || inferKimakiTunnelId(publicUrl, options.kimakiDomain || KIMAKI_DEFAULT_DOMAIN);
  const args = ['tunnel', '--port', localUrl.port || defaultPort(localUrl), '--host', localUrl.hostname];

  if (tunnelId) {
    args.push('--tunnel-id', tunnelId);
  }
  if (options.server) {
    args.push('--server', options.server);
  }

  return {
    command: options.kimakiBin || process.env.HOMEBOY_KIMAKI_BIN || 'kimaki',
    args,
  };
}

function evaluateHostPreservation({
  localUrl,
  publicUrl,
  expectedEffectiveOrigin,
  expectedConfigHostname,
  requireHostPreservation,
  provider,
}) {
  if (!requireHostPreservation) {
    return {
      required: false,
      supported: true,
      mode: 'public-url-only',
    };
  }

  const expectedOrigin = expectedEffectiveOrigin || localUrl.origin;
  const expectedHostname = expectedConfigHostname || localUrl.hostname;
  const publicOriginMatches = publicUrl.origin === expectedOrigin;
  const publicHostnameMatches = publicUrl.hostname === expectedHostname;

  if (publicOriginMatches && publicHostnameMatches) {
    return {
      required: true,
      supported: true,
      mode: 'public-url-preserves-browser-origin',
      expected_effective_origin: expectedOrigin,
      expected_config_hostname: expectedHostname,
    };
  }

  return {
    required: true,
    supported: false,
    mode: 'unsupported-browser-origin-change',
    reason: `${provider} exposes ${publicUrl.origin}, but this workload requires browser-effective origin ${expectedOrigin} and config hostname ${expectedHostname}.`,
    recommendation: 'Use a backend/broker that can provide reviewer access without changing the browser-effective origin, or keep this workload on a local browser probe until that ingress exists.',
    expected_effective_origin: expectedOrigin,
    expected_config_hostname: expectedHostname,
  };
}

function inferKimakiTunnelId(publicUrl, domain) {
  const suffix = `.${domain}`;
  if (!publicUrl.hostname.endsWith(suffix)) {
    return '';
  }
  return publicUrl.hostname.slice(0, -suffix.length);
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

function defaultPort(url) {
  if (url.protocol === 'https:') {
    return '443';
  }
  return '80';
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
