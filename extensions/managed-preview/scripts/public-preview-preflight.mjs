#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { mkdir, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { dirname } from 'node:path';
import { URL } from 'node:url';

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const previewPort = await resolvePreviewPort(options);
  const localUrl = resolveLocalUrl(options, previewPort);
  const publicUrlValue = options.publicUrl || process.env.HOMEBOY_PUBLIC_PREVIEW_URL || process.env.HOMEBOY_TUNNEL_PUBLIC_URL || '';
  const publicUrl = publicUrlValue ? parseUrl(publicUrlValue, 'public URL') : null;

  if (!options.prepareOnly && !publicUrl) {
    throw statusError('missing_public_url', 'Missing public URL');
  }

  if (publicUrl && publicUrl.protocol !== 'https:' && !options.allowInsecurePublicUrl) {
    throw statusError('public_url_not_https', 'Public preview URL must use HTTPS. Pass --allow-insecure-public-url only for local smoke tests.');
  }

  if (Number(localUrl.port || defaultPort(localUrl)) !== previewPort) {
    throw statusError('local_port_mismatch', `Local preview URL port ${localUrl.port || defaultPort(localUrl)} does not match selected preview port ${previewPort}.`);
  }

  const preflightPath = options.preflightPath || process.env.HOMEBOY_PUBLIC_PREVIEW_PREFLIGHT_PATH || '/';
  if (options.prepareOnly) {
    const metadata = buildPreparedMetadata({ options, previewPort, localUrl, publicUrl, preflightPath });
    await writeMetadata(options.metadataFile, metadata);
    process.stdout.write(`${JSON.stringify(metadata, null, 2)}\n`);
    return;
  }

  const timeoutMs = Number(options.timeoutMs || process.env.HOMEBOY_PUBLIC_PREVIEW_TIMEOUT_MS || 5000);
  const localProbeUrl = withPath(localUrl, preflightPath);
  const publicProbeUrl = withPath(publicUrl, preflightPath);
  const expectedStatus = options.expectedStatus ? Number(options.expectedStatus) : null;
  const expectedText = options.expectedText || process.env.HOMEBOY_PUBLIC_PREVIEW_EXPECT_TEXT || '';

  const localProbe = await probe(localProbeUrl, timeoutMs);
  const publicProbe = await probe(publicProbeUrl, timeoutMs);

  if (!localProbe.ok) {
    throw statusError('local_preflight_failed', `Local preview preflight failed for selected port ${previewPort}: ${localProbe.error || `HTTP ${localProbe.status}`}`);
  }
  if (!publicProbe.ok) {
    throw statusError('public_preflight_failed', `Public preview preflight failed before browser trace launch: ${publicProbe.error || `HTTP ${publicProbe.status}`}`);
  }
  if (expectedStatus !== null && publicProbe.status !== expectedStatus) {
    throw statusError('public_status_mismatch', `Public preview returned HTTP ${publicProbe.status}; expected ${expectedStatus}.`);
  }
  if (expectedStatus === null && publicProbe.status !== localProbe.status) {
    throw statusError('public_local_status_mismatch', `Public preview returned HTTP ${publicProbe.status}; local selected port returned HTTP ${localProbe.status}.`);
  }
  if (expectedText && (!localProbe.body.includes(expectedText) || !publicProbe.body.includes(expectedText))) {
    throw statusError('expected_text_missing', 'Expected preflight text was not present in both local and public preview responses.');
  }

  const metadata = buildMetadata({
    options,
    previewPort,
    localUrl,
    publicUrl,
    localProbe,
    publicProbe,
    preflightPath,
  });

  if (options.metadataFile) {
    await writeMetadata(options.metadataFile, metadata);
  }

  process.stdout.write(`${JSON.stringify(metadata, null, 2)}\n`);
}

async function writeMetadata(metadataFile, metadata) {
  if (!metadataFile) {
    return;
  }
  await mkdir(dirname(metadataFile), { recursive: true });
  await writeFile(metadataFile, `${JSON.stringify(metadata, null, 2)}\n`);
}

async function resolvePreviewPort(options) {
  const explicit = options.previewPort || options.runtimePort || process.env.HOMEBOY_PUBLIC_PREVIEW_PORT || process.env.HOMEBOY_RUNTIME_PREVIEW_PORT;
  if (explicit) {
    return parsePort(explicit, 'preview port');
  }
  if (options.localUrl || process.env.HOMEBOY_LOCAL_PREVIEW_URL || process.env.HOMEBOY_SERVICE_LOCAL_URL) {
    const localUrl = parseUrl(options.localUrl || process.env.HOMEBOY_LOCAL_PREVIEW_URL || process.env.HOMEBOY_SERVICE_LOCAL_URL, 'local URL');
    return parsePort(localUrl.port || defaultPort(localUrl), 'local URL port');
  }
  if (options.allocatePort) {
    return allocatePort(options.host || process.env.HOMEBOY_PUBLIC_PREVIEW_HOST || '127.0.0.1');
  }
  throw statusError('missing_preview_port', 'Pass --preview-port, --runtime-port, --local-url, or --allocate-port.');
}

function resolveLocalUrl(options, previewPort) {
  const value = options.localUrl || process.env.HOMEBOY_LOCAL_PREVIEW_URL || process.env.HOMEBOY_SERVICE_LOCAL_URL;
  if (value) {
    return parseUrl(value, 'local URL');
  }
  const host = options.host || process.env.HOMEBOY_PUBLIC_PREVIEW_HOST || '127.0.0.1';
  return parseUrl(`http://${host}:${previewPort}`, 'local URL');
}

function allocatePort(host) {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.on('error', reject);
    server.listen(0, host, () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(port);
      });
    });
  });
}

async function probe(url, timeoutMs) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(url.href, { signal: controller.signal, redirect: 'manual' });
    const body = await response.text();
    return {
      ok: response.status >= 200 && response.status < 500,
      status: response.status,
      content_type: response.headers.get('content-type') || null,
      body,
      body_bytes: Buffer.byteLength(body),
      body_sha256: createHash('sha256').update(body).digest('hex'),
    };
  } catch (error) {
    return {
      ok: false,
      status: null,
      content_type: null,
      body: '',
      body_bytes: 0,
      body_sha256: null,
      error: error.name === 'AbortError' ? `Timed out after ${timeoutMs}ms` : error.message,
    };
  } finally {
    clearTimeout(timer);
  }
}

function buildMetadata({ options, previewPort, localUrl, publicUrl, localProbe, publicProbe, preflightPath }) {
  return {
    schema: 'homeboy/public-preview-preflight/v1',
    status: 'ready',
    lifecycle_boundary: 'Homeboy Extensions validates and records the public preview contract; the caller or Homeboy core owns tunnel process lifecycle.',
    local_preview: {
      origin: localUrl.origin,
      url: redactUrl(localUrl),
      host: localUrl.hostname,
      port: previewPort,
    },
    public_preview: {
      url: redactUrl(publicUrl),
      origin: publicUrl.origin,
      provider: options.tunnelProvider || process.env.HOMEBOY_PUBLIC_PREVIEW_PROVIDER || process.env.HOMEBOY_TUNNEL_PROVIDER || null,
      session_id: options.tunnelSessionId || process.env.HOMEBOY_PUBLIC_PREVIEW_SESSION_ID || process.env.HOMEBOY_TUNNEL_SESSION_ID || null,
    },
    preflight: {
      path: preflightPath,
      local: redactProbe(localProbe),
      public: redactProbe(publicProbe),
    },
    artifact_policy: {
      publish_raw: true,
      secret_fields: [],
      note: 'Probe bodies, credentials, query strings, and URL fragments are intentionally omitted.',
    },
  };
}

function buildPreparedMetadata({ options, previewPort, localUrl, publicUrl, preflightPath }) {
  return {
    schema: 'homeboy/public-preview-preflight/v1',
    status: 'prepared',
    lifecycle_boundary: 'Homeboy Extensions selected the preview port and recorded the public preview contract; the caller must start the runtime/tunnel and rerun without --prepare-only before browser launch.',
    local_preview: {
      origin: localUrl.origin,
      url: redactUrl(localUrl),
      host: localUrl.hostname,
      port: previewPort,
    },
    public_preview: publicUrl ? {
      url: redactUrl(publicUrl),
      origin: publicUrl.origin,
      provider: options.tunnelProvider || process.env.HOMEBOY_PUBLIC_PREVIEW_PROVIDER || process.env.HOMEBOY_TUNNEL_PROVIDER || null,
      session_id: options.tunnelSessionId || process.env.HOMEBOY_PUBLIC_PREVIEW_SESSION_ID || process.env.HOMEBOY_TUNNEL_SESSION_ID || null,
    } : null,
    preflight: {
      path: preflightPath,
      local: null,
      public: null,
    },
    artifact_policy: {
      publish_raw: true,
      secret_fields: [],
      note: 'Probe bodies, credentials, query strings, and URL fragments are intentionally omitted.',
    },
  };
}

function redactProbe(probeResult) {
  return {
    ok: probeResult.ok,
    status: probeResult.status,
    content_type: probeResult.content_type,
    body_bytes: probeResult.body_bytes,
    body_sha256: probeResult.body_sha256,
    error: probeResult.error || null,
  };
}

function withPath(url, path) {
  const next = new URL(url.href);
  next.pathname = path.startsWith('/') ? path : `/${path}`;
  next.search = '';
  next.hash = '';
  return next;
}

function redactUrl(url) {
  const next = new URL(url.href);
  next.username = '';
  next.password = '';
  next.search = '';
  next.hash = '';
  return next.href;
}

function parseUrl(value, label) {
  if (!value) {
    throw statusError(`missing_${slug(label)}`, `Missing ${label}`);
  }
  try {
    return new URL(value);
  } catch (_error) {
    throw statusError(`invalid_${slug(label)}`, `Invalid ${label}: ${value}`);
  }
}

function parsePort(value, label) {
  const port = Number(value);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw statusError(`invalid_${slug(label)}`, `Invalid ${label}: ${value}`);
  }
  return port;
}

function defaultPort(url) {
  if (url.protocol === 'https:') {
    return '443';
  }
  if (url.protocol === 'http:') {
    return '80';
  }
  return '';
}

function statusError(code, message) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function parseArgs(args) {
  const options = {};
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === '--allocate-port') {
      options.allocatePort = true;
      continue;
    }
    if (arg === '--allow-insecure-public-url') {
      options.allowInsecurePublicUrl = true;
      continue;
    }
    if (arg === '--prepare-only') {
      options.prepareOnly = true;
      continue;
    }
    if (!arg.startsWith('--')) {
      throw statusError('unexpected_argument', `Unexpected positional argument: ${arg}`);
    }
    const key = toCamelCase(arg.slice(2));
    const value = args[i + 1];
    if (!value || value.startsWith('--')) {
      throw statusError('missing_argument_value', `Missing value for ${arg}`);
    }
    options[key] = value;
    i += 1;
  }
  return options;
}

function toCamelCase(value) {
  return value.replace(/-([a-z])/g, (_, char) => char.toUpperCase());
}

function slug(value) {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '');
}

main().catch((error) => {
  process.stderr.write(`${JSON.stringify({
    schema: 'homeboy/public-preview-preflight-error/v1',
    status: 'failed',
    code: error.code || 'public_preview_preflight_failed',
    error: error.message,
  }, null, 2)}\n`);
  process.exitCode = 1;
});
