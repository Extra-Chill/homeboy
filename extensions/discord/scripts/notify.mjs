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
    route_kind: validation.route_kind,
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
  // Homeboy appends --transport alongside --route whenever the caller
  // selected a transport explicitly. This extension was already chosen by that
  // id, so the value carries no new information — but rejecting the flag makes
  // every explicitly-routed notification fail.
  const names = new Set(['run-id', 'status', 'title', 'body', 'route', 'transport']);
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token === '--dry-run') {
      args.dryRun = true;
      continue;
    }
    if (!token.startsWith('--')) {
      args.error = 'Expected --run-id, --status, --title, and --body; --transport, --route and --dry-run are optional.';
      return args;
    }
    const [flag, inlineValue] = token.slice(2).split(/=(.*)/s, 2);
    if (!names.has(flag) || (inlineValue === undefined && index + 1 >= tokens.length)) {
      args.error = 'Expected --run-id, --status, --title, and --body; --transport, --route and --dry-run are optional.';
      return args;
    }
    const name = flag.replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    args[name] = inlineValue === undefined ? tokens[index + 1] : inlineValue;
    if (inlineValue === undefined) index += 1;
  }
  return args;
}

function validate(args, env) {
  if (args.error || !nonEmpty(args.runId) || !nonEmpty(args.status) || !nonEmpty(args.title) || !nonEmpty(args.body)) {
    return { ok: false, error: args.error || 'run-id, status, title, and body must be non-empty.' };
  }

  const botToken = value(env.DISCORD_BOT_TOKEN) || value(env.KIMAKI_BOT_TOKEN);
  const webhookUrl = value(env.DISCORD_WEBHOOK_URL);
  if (Boolean(botToken) === Boolean(webhookUrl)) {
    return { ok: false, error: 'Configure exactly one auth mode: DISCORD_BOT_TOKEN or DISCORD_WEBHOOK_URL.' };
  }

  const route = value(args.route);
  const parsedRoute = route === undefined ? undefined : parseRoute(route);
  if (parsedRoute?.error) return { ok: false, error: parsedRoute.error };

  const operationsChannelId = value(env.DISCORD_OPERATIONS_CHANNEL_ID);
  if (botToken) {
    const resolved = resolveBotDestination(parsedRoute, operationsChannelId);
    if (resolved.error) return { ok: false, error: resolved.error };
    const apiBase = value(env.DISCORD_API_BASE_URL) || 'https://discord.com/api/v10';
    let url;
    try {
      url = new URL(`channels/${encodeURIComponent(resolved.id)}/messages`, ensureApiBase(apiBase));
    } catch {
      return { ok: false, error: 'DISCORD_API_BASE_URL must be a valid HTTP(S) URL.' };
    }
    if (!isHttp(url)) return { ok: false, error: 'DISCORD_API_BASE_URL must be an HTTP(S) URL.' };
    return { ok: true, mode: 'bot', ...resolved, url, headers: { authorization: `Bot ${botToken}`, 'content-type': 'application/json' } };
  }

  if (operationsChannelId) return { ok: false, error: 'Webhook mode does not use DISCORD_OPERATIONS_CHANNEL_ID.' };
  if (parsedRoute?.kind === 'channel') return { ok: false, error: 'A channel route requires DISCORD_BOT_TOKEN; webhook delivery can only target its configured webhook channel or a thread.' };
  let url;
  try {
    url = new URL(webhookUrl);
  } catch {
    return { ok: false, error: 'DISCORD_WEBHOOK_URL must be a valid HTTP(S) URL.' };
  }
  if (!isHttp(url)) return { ok: false, error: 'DISCORD_WEBHOOK_URL must be an HTTP(S) URL.' };
  url.searchParams.set('wait', 'true');
  if (parsedRoute?.kind === 'thread') url.searchParams.set('thread_id', parsedRoute.id);
  return {
    ok: true,
    mode: 'webhook',
    route_kind: parsedRoute?.kind || 'webhook',
    destination: parsedRoute ? 'dynamic_thread' : 'webhook_default',
    url,
    headers: { 'content-type': 'application/json' },
  };
}

function resolveBotDestination(route, operationsChannelId) {
  if (route) return { id: route.id, route_kind: route.kind, destination: `dynamic_${route.kind}` };
  if (!operationsChannelId) return { error: 'Bot mode without --route requires DISCORD_OPERATIONS_CHANNEL_ID.' };
  if (!isSnowflake(operationsChannelId)) return { error: 'DISCORD_OPERATIONS_CHANNEL_ID must be a Discord snowflake.' };
  return { id: operationsChannelId, route_kind: 'operations', destination: 'operations_channel' };
}

function parseRoute(route) {
  // Canonical guild-less form emitted by the kimaki notification bridge
  // (wp-coding-agents #261): discord:v1:<channel|thread>:<destination-id>.
  const canonical = /^discord:v1:(channel|thread):(\d{17,20})$/.exec(route);
  if (canonical) return { kind: canonical[1], id: canonical[2] };
  // Legacy 4-segment form with a guild id. The guild is not used for delivery
  // (only the destination id is), so accept it for backward compatibility.
  const legacy = /^discord:v1:(channel|thread):(\d{17,20}):(\d{17,20})$/.exec(route);
  if (legacy) return { kind: legacy[1], id: legacy[3] };
  return { error: 'route must be discord:v1:<channel|thread>:<destination-id> with Discord snowflake IDs.' };
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

function isSnowflake(input) {
  return typeof input === 'string' && /^\d{17,20}$/.test(input);
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

await main();
