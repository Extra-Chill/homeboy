#!/usr/bin/env node

const DISCORD_CONTENT_LIMIT = 2000;
const MAX_RETRIES = 2;
const MAX_RETRY_DELAY_MS = 5000;

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const validation = validate(args, process.env);
  if (!validation.ok) {
    return finish(failure('input_error', validation.error));
  }

  const rendered = renderContent(args);
  const proof = {
    mode: validation.mode,
    destination: validation.destination,
    content_length: rendered.content.length,
    truncated: rendered.truncated,
  };

  if (args.dryRun) {
    return finish({
      schema: 'homeboy/discord-notification-result/v1',
      status: 'dry_run',
      delivery: proof,
      attempts: 0,
    });
  }

  let attempts = 0;
  for (;;) {
    attempts += 1;
    let response;
    try {
      response = await fetch(validation.url, {
        method: 'POST',
        headers: validation.headers,
        body: JSON.stringify({ content: rendered.content }),
      });
    } catch {
      return finish(failure('delivery_error', 'Discord request could not be completed.', proof, attempts));
    }

    if (response.ok) {
      return finish({
        schema: 'homeboy/discord-notification-result/v1',
        status: 'delivered',
        delivery: proof,
        attempts,
      });
    }

    if (response.status === 429 && attempts <= MAX_RETRIES) {
      const retryAfterMs = await retryAfter(response);
      await sleep(retryAfterMs);
      continue;
    }

    return finish(failure(classify(response.status), errorFor(response.status), proof, attempts, response.status));
  }
}

function parseArgs(tokens) {
  const args = { dryRun: false };
  const names = new Set(['run-id', 'status', 'title', 'body']);
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token === '--dry-run') {
      args.dryRun = true;
      continue;
    }
    if (!token.startsWith('--') || !names.has(token.slice(2)) || index + 1 >= tokens.length) {
      args.error = 'Expected --run-id, --status, --title, and --body; --dry-run is optional.';
      return args;
    }
    const name = token.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    args[name] = tokens[index + 1];
    index += 1;
  }
  return args;
}

function validate(args, env) {
  if (args.error || !nonEmpty(args.runId) || !nonEmpty(args.status) || !nonEmpty(args.title) || !nonEmpty(args.body)) {
    return { ok: false, error: args.error || 'run-id, status, title, and body must be non-empty.' };
  }

  const botToken = value(env.DISCORD_BOT_TOKEN);
  const webhookUrl = value(env.DISCORD_WEBHOOK_URL);
  if (Boolean(botToken) === Boolean(webhookUrl)) {
    return { ok: false, error: 'Configure exactly one auth mode: DISCORD_BOT_TOKEN or DISCORD_WEBHOOK_URL.' };
  }

  const channelId = value(env.DISCORD_CHANNEL_ID);
  const threadId = value(env.DISCORD_THREAD_ID);
  if (botToken) {
    if (Boolean(channelId) === Boolean(threadId)) {
      return { ok: false, error: 'Bot mode requires exactly one destination: DISCORD_CHANNEL_ID or DISCORD_THREAD_ID.' };
    }
    const destination = threadId ? 'thread' : 'channel';
    const apiBase = value(env.DISCORD_API_BASE_URL) || 'https://discord.com/api/v10';
    let url;
    try {
      url = new URL(`channels/${encodeURIComponent(threadId || channelId)}/messages`, ensureApiBase(apiBase));
    } catch {
      return { ok: false, error: 'DISCORD_API_BASE_URL must be a valid HTTP(S) URL.' };
    }
    if (!isHttp(url)) return { ok: false, error: 'DISCORD_API_BASE_URL must be an HTTP(S) URL.' };
    return { ok: true, mode: 'bot', destination, url, headers: { authorization: `Bot ${botToken}`, 'content-type': 'application/json' } };
  }

  if (channelId) return { ok: false, error: 'Webhook mode does not use DISCORD_CHANNEL_ID.' };
  let url;
  try {
    url = new URL(webhookUrl);
  } catch {
    return { ok: false, error: 'DISCORD_WEBHOOK_URL must be a valid HTTP(S) URL.' };
  }
  if (!isHttp(url)) return { ok: false, error: 'DISCORD_WEBHOOK_URL must be an HTTP(S) URL.' };
  url.searchParams.set('wait', 'true');
  if (threadId) url.searchParams.set('thread_id', threadId);
  return { ok: true, mode: 'webhook', destination: threadId ? 'thread' : 'webhook', url, headers: { 'content-type': 'application/json' } };
}

function renderContent(args) {
  const content = `[${compact(args.status)}] ${compact(args.title)}\nRun: ${compact(args.runId)}\n${compact(args.body)}`;
  if (content.length <= DISCORD_CONTENT_LIMIT) return { content, truncated: false };
  let bounded = '';
  for (const character of content) {
    if (bounded.length + character.length > DISCORD_CONTENT_LIMIT - 3) break;
    bounded += character;
  }
  return { content: `${bounded}...`, truncated: true };
}

async function retryAfter(response) {
  let retryAfterSeconds = 0;
  try {
    const body = await response.json();
    retryAfterSeconds = Number(body.retry_after);
  } catch {}
  return Math.min(MAX_RETRY_DELAY_MS, Math.max(0, Number.isFinite(retryAfterSeconds) ? retryAfterSeconds * 1000 : 0));
}

function classify(status) {
  if (status === 401 || status === 403) return 'auth_error';
  if (status === 400 || status === 404 || status === 405 || status === 413 || status === 422) return 'input_error';
  return 'delivery_error';
}

function errorFor(status) {
  if (status === 401 || status === 403) return 'Discord rejected the configured credentials.';
  if (status === 400 || status === 404 || status === 405 || status === 413 || status === 422) return 'Discord rejected the notification input or destination.';
  if (status === 429) return 'Discord rate limit retries were exhausted.';
  return 'Discord returned an unexpected delivery failure.';
}

function failure(kind, error, delivery = undefined, attempts = 0, httpStatus = undefined) {
  return {
    schema: 'homeboy/discord-notification-result/v1',
    status: 'failed',
    error: { kind, message: error, ...(httpStatus === undefined ? {} : { http_status: httpStatus }) },
    ...(delivery === undefined ? {} : { delivery }),
    attempts,
  };
}

function finish(result) {
  process.stdout.write(`${JSON.stringify(result)}\n`);
  process.exitCode = result.status === 'failed' ? 1 : 0;
}

function ensureApiBase(base) {
  return base.endsWith('/') ? base : `${base}/`;
}

function compact(text) {
  return String(text).replace(/\s+/g, ' ').trim();
}

function value(input) {
  return typeof input === 'string' && input.trim() ? input.trim() : undefined;
}

function nonEmpty(input) {
  return value(input) !== undefined;
}

function isHttp(url) {
  return url.protocol === 'https:' || url.protocol === 'http:';
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

await main();
