// Invocation-scoped chat session context, shared by the route resolver and the
// notification helper.
//
// A chat bridge that runs agent sessions describes the session that invoked
// Homeboy through a bridge-neutral environment contract:
//
//   HOMEBOY_SESSION_THREAD_ID        Discord thread that owns this session.
//                                    The route resolver turns it into the
//                                    run's route.
//   HOMEBOY_SESSION_SEND_COMMAND     Command that delivers a prompt into a
//                                    session thread as a real turn. Invoked without a
//                                    shell as `<command...> --thread <id>
//                                    --prompt <text>`.
//   HOMEBOY_SESSION_SEND_URL         Or: an HTTP endpoint that does the same.
//                                    POST {"options":{"thread","prompt"}} with
//                                    a bearer token; a 2xx response, optionally
//                                    NDJSON carrying a final {"exit":<code>}.
//   HOMEBOY_SESSION_SEND_TOKEN       Bearer token for the HTTP sender, or
//   HOMEBOY_SESSION_SEND_TOKEN_FILE  a file holding it.
//
// The bridge's installer maps its own variables onto this contract; this
// extension names no bridge.

import fs from 'node:fs';

// Deprecated bridge-specific names accepted for one release. Each maps to its
// replacement and is reported in the result envelope while in use.
const DEPRECATED_ALIASES = {
  thread: { name: 'KIMAKI_THREAD_ID', replacement: 'HOMEBOY_SESSION_THREAD_ID' },
  command: { name: 'KIMAKI_CLI', replacement: 'HOMEBOY_SESSION_SEND_COMMAND' },
  botToken: { name: 'KIMAKI_BOT_TOKEN', replacement: 'DISCORD_BOT_TOKEN' },
};
const DEPRECATED_DEFAULT_SEND_COMMAND = ['kimaki', 'send'];

export function value(input) {
  return typeof input === 'string' && input.trim() ? input.trim() : undefined;
}

export function deprecation(key) {
  const alias = DEPRECATED_ALIASES[key];
  return { name: alias.name, replacement: alias.replacement };
}

/** Bot token from the deprecated alias, when the current name is unset. */
export function deprecatedBotToken(env) {
  return value(env[DEPRECATED_ALIASES.botToken.name]);
}

/** The invoking session's thread id, and whether a deprecated alias supplied it. */
export function sessionThread(env) {
  const threadId = value(env.HOMEBOY_SESSION_THREAD_ID);
  if (threadId !== undefined) return { threadId, deprecated: false };
  const legacy = value(env[DEPRECATED_ALIASES.thread.name]);
  if (legacy !== undefined) return { threadId: legacy, deprecated: true };
  return { threadId: undefined, deprecated: false };
}

/**
 * The configured session sender.
 *
 * Resolves to `{ sender }` with `{ kind: 'command', argv }` or
 * `{ kind: 'http', url, token }`, to `{ sender: undefined }` when none is
 * configured, or to `{ error }` when the configuration is unusable. Errors
 * never include the token, its file path, or the endpoint.
 */
export function sessionSender(env) {
  const deprecations = [];
  const command = value(env.HOMEBOY_SESSION_SEND_COMMAND);
  const url = value(env.HOMEBOY_SESSION_SEND_URL);
  if (command !== undefined && url !== undefined) {
    return { error: 'Configure at most one session sender: HOMEBOY_SESSION_SEND_COMMAND or HOMEBOY_SESSION_SEND_URL.' };
  }

  if (url !== undefined) {
    let parsed;
    try {
      parsed = new URL(url);
    } catch {
      return { error: 'HOMEBOY_SESSION_SEND_URL must be a valid HTTP(S) URL.' };
    }
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
      return { error: 'HOMEBOY_SESSION_SEND_URL must be an HTTP(S) URL.' };
    }
    const token = sessionToken(env);
    if (token.error) return { error: token.error };
    return { sender: { kind: 'http', url: parsed, token: token.token }, deprecations };
  }

  if (command !== undefined) {
    return { sender: { kind: 'command', argv: command.split(/\s+/) }, deprecations };
  }

  // Deprecated: a host that exposed the legacy thread or CLI alias was
  // delivered through that bridge's own CLI. Kept for one release so existing
  // installs keep working until their installer exports the contract above.
  const legacyThread = value(env[DEPRECATED_ALIASES.thread.name]);
  const legacyCommand = value(env[DEPRECATED_ALIASES.command.name]);
  if (legacyThread !== undefined || legacyCommand !== undefined) {
    if (legacyThread !== undefined) deprecations.push(deprecation('thread'));
    if (legacyCommand !== undefined) deprecations.push(deprecation('command'));
    return {
      sender: {
        kind: 'command',
        argv: legacyCommand === undefined ? [...DEPRECATED_DEFAULT_SEND_COMMAND] : [legacyCommand, 'send'],
      },
      deprecations,
    };
  }

  return { sender: undefined, deprecations };
}

function sessionToken(env) {
  const token = value(env.HOMEBOY_SESSION_SEND_TOKEN);
  const tokenFile = value(env.HOMEBOY_SESSION_SEND_TOKEN_FILE);
  if (token !== undefined && tokenFile !== undefined) {
    return { error: 'Configure at most one of HOMEBOY_SESSION_SEND_TOKEN or HOMEBOY_SESSION_SEND_TOKEN_FILE.' };
  }
  if (token !== undefined) return { token };
  if (tokenFile === undefined) {
    return { error: 'HOMEBOY_SESSION_SEND_URL requires HOMEBOY_SESSION_SEND_TOKEN or HOMEBOY_SESSION_SEND_TOKEN_FILE.' };
  }
  let contents;
  try {
    contents = fs.readFileSync(tokenFile, 'utf8');
  } catch {
    return { error: 'HOMEBOY_SESSION_SEND_TOKEN_FILE could not be read.' };
  }
  const fromFile = value(contents);
  if (fromFile === undefined) return { error: 'HOMEBOY_SESSION_SEND_TOKEN_FILE is empty.' };
  return { token: fromFile };
}
