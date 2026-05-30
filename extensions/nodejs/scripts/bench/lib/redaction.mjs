import { readFile, writeFile } from 'node:fs/promises';

const REDACTED = '[REDACTED]';

const WEB_SECRET_KEYS = [
    /^authorization$/i,
    /^proxy-authorization$/i,
    /^cookie$/i,
    /^set-cookie$/i,
    /^x-api-key$/i,
    /^x-auth-token$/i,
    /^x-csrf-token$/i,
    /^x-xsrf-token$/i,
    /(?:^|[-_])(access|refresh|id)?token(?:$|[-_])/i,
    /(?:^|[-_])api[-_]?key(?:$|[-_])/i,
    /(?:^|[-_])client[-_]?secret(?:$|[-_])/i,
    /(?:^|[-_])(password|passwd|pwd|secret|session|nonce|csrf|xsrf|credential)(?:$|[-_])/i,
];

const WEB_QUERY_SECRET_KEYS = [
    ...WEB_SECRET_KEYS,
    /^key$/i,
    /^code$/i,
    /^sig$/i,
    /^signature$/i,
    /^auth$/i,
];

const PROFILES = {
    web: {
        secretKeys: WEB_SECRET_KEYS,
        querySecretKeys: WEB_QUERY_SECRET_KEYS,
    },
};

export function sanitizeArtifactValue(value, options = {}) {
    const config = normalizeRedactionOptions(options);
    return sanitizeValue(value, config, new WeakSet(), '');
}

export function redactText(value, options = {}) {
    if (typeof value !== 'string' || value.length === 0) return value;

    const config = normalizeRedactionOptions(options);
    let redacted = value.replace(/https?:\/\/[^\s"'<>)]*/gi, (url) => sanitizeUrl(url, config));
    redacted = redacted.replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+/gi, `$1 ${config.replacement}`);
    redacted = redacted.replace(/\b([A-Za-z0-9_.-]*(?:token|secret|password|passwd|pwd|nonce|session|cookie|credential|api[-_]?key)[A-Za-z0-9_.-]*)=([^\s&;,]+)/gi, (_match, key) => `${key}=${config.replacement}`);
    return redacted;
}

export function sanitizeUrl(value, options = {}) {
    if (typeof value !== 'string' || value.length === 0) return value;

    const config = options.querySecretKeys ? options : normalizeRedactionOptions(options);
    try {
        const url = new URL(value);
        for (const key of [...url.searchParams.keys()]) {
            if (matchesAny(key, config.querySecretKeys)) {
                url.searchParams.set(key, config.replacement);
            }
        }
        if (url.username) url.username = config.replacement;
        if (url.password) url.password = config.replacement;
        return url.toString();
    } catch {
        return value;
    }
}

export async function sanitizeArtifactFile(file, options = {}) {
    const input = await readFile(file, 'utf8');
    let output;

    try {
        output = JSON.stringify(sanitizeArtifactValue(JSON.parse(input), options), null, 2);
        if (input.endsWith('\n')) output += '\n';
    } catch {
        output = redactText(input, options);
    }

    await writeFile(file, output);
    return { path: file };
}

export function normalizeJsonValue(value) {
    return JSON.parse(JSON.stringify(value, (_key, item) => {
        if (typeof item === 'number' && !Number.isFinite(item)) return null;
        if (typeof item === 'bigint') return item.toString();
        return item;
    }));
}

function sanitizeValue(value, config, seen, key) {
    if (typeof value === 'number') return Number.isFinite(value) ? value : null;
    if (typeof value === 'bigint') return value.toString();
    if (typeof value === 'string') {
        if (matchesAny(key, config.secretKeys)) return config.replacement;
        return redactText(value, config);
    }
    if (value === null || typeof value !== 'object') return value;
    if (seen.has(value)) return '[Circular]';
    seen.add(value);

    if (Array.isArray(value)) {
        const out = value.map((item) => sanitizeValue(item, config, seen, key));
        seen.delete(value);
        return out;
    }

    const out = {};
    for (const [entryKey, entryValue] of Object.entries(value)) {
        out[entryKey] = matchesAny(entryKey, config.secretKeys)
            ? config.replacement
            : sanitizeValue(entryValue, config, seen, entryKey);
    }
    seen.delete(value);
    return out;
}

function normalizeRedactionOptions(options) {
    const profile = options.profile || 'web';
    const base = PROFILES[profile] || PROFILES.web;
    return {
        replacement: options.replacement || REDACTED,
        secretKeys: [...base.secretKeys, ...(options.secretKeys || [])],
        querySecretKeys: [...base.querySecretKeys, ...(options.querySecretKeys || [])],
    };
}

function matchesAny(key, patterns) {
    if (!key) return false;
    return patterns.some((pattern) => pattern instanceof RegExp ? pattern.test(key) : pattern === key);
}
