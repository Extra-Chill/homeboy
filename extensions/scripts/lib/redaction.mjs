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

const WEB_QUERY_SECRET_KEYS = [...WEB_SECRET_KEYS, /^key$/i, /^code$/i, /^sig$/i, /^signature$/i, /^auth$/i];
const PROFILES = { web: { secretKeys: WEB_SECRET_KEYS, querySecretKeys: WEB_QUERY_SECRET_KEYS } };
const TEXT_SECRET_KEY = /authorization|proxy-authorization|token|api[-_]?key|client[-_]?secret|password|passwd|pwd|secret|session|nonce|cookie|credential/i;

export function sanitizeArtifactValue(value, options = {}) {
    return sanitizeValue(value, normalizeRedactionOptions(options), new WeakSet(), '');
}

export function redactText(value, options = {}) {
    if (typeof value !== 'string' || value.length === 0) return value;
    const config = normalizeRedactionOptions(options);
    let redacted = value.replace(/https?:\/\/[^\s"'<>)]*/gi, (url) => sanitizeUrl(url, config));
    redacted = redacted.replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+/gi, `$1 ${config.replacement}`);
    return redactAssignments(redacted, config.replacement);
}

function redactAssignments(value, replacement) {
    const key = /\b([A-Za-z0-9_.-]+)(?:\\*["'])?\s*[:=]\s*/g;
    let output = '';
    let cursor = 0;
    let match;
    while ((match = key.exec(value))) {
        if (!TEXT_SECRET_KEY.test(match[1])) continue;
        const start = key.lastIndex;
        const opening = value.slice(start).match(/^\\*(["'])/);
        let end = start;
        let wrapper = '';
        if (opening) {
            wrapper = opening[0];
            const quote = opening[1];
            const escaped = wrapper.length - 1;
            end += wrapper.length;
            while (end < value.length) {
                if (value[end] === quote) {
                    let slashes = 0;
                    for (let index = end - 1; index >= start && value[index] === '\\'; index--) slashes++;
                    // Encoded closing quotes and escaped quotes within a value
                    // have different backslash parity at each JSON layer.
                    if (slashes % (2 * (escaped + 1)) === escaped) {
                        end++;
                        break;
                    }
                }
                end++;
            }
        } else {
            while (end < value.length && !/[\s&;,}\]"']/.test(value[end])) end++;
        }
        output += value.slice(cursor, start) + wrapper + replacement + wrapper;
        cursor = end;
        key.lastIndex = end;
    }
    return output + value.slice(cursor);
}

// Bound only already-sanitized text, so an unlabelled suffix of a long secret
// can never cross the publication boundary.
export function boundTextUtf8(value, maxBytes = 4096) {
    if (!Number.isSafeInteger(maxBytes) || maxBytes < 128) {
        throw new RangeError('Excerpt byte limit must be an integer of at least 128');
    }
    const text = String(value || '');
    const encoded = Buffer.from(text, 'utf8');
    if (encoded.length <= maxBytes) return text;
    let retainedBytes = maxBytes;
    while (true) {
        const requestedBytes = retainedBytes;
        let start = encoded.length - retainedBytes;
        while (start < encoded.length && (encoded[start] & 0xc0) === 0x80) start += 1;
        retainedBytes = encoded.length - start;
        const omitted = encoded.length - retainedBytes;
        const marker = `[truncated ${omitted} UTF-8 bytes; retained tail]\n`;
        const nextRetainedBytes = maxBytes - Buffer.byteLength(marker, 'utf8');
        if (nextRetainedBytes === requestedBytes) return marker + encoded.subarray(start).toString('utf8');
        retainedBytes = nextRetainedBytes;
    }
}

export function sanitizeUrl(value, options = {}) {
    if (typeof value !== 'string' || value.length === 0) return value;
    const config = options.querySecretKeys ? options : normalizeRedactionOptions(options);
    try {
        const url = new URL(value);
        for (const key of [...url.searchParams.keys()]) if (matchesAny(key, config.querySecretKeys)) url.searchParams.set(key, config.replacement);
        if (url.username) url.username = config.replacement;
        if (url.password) url.password = config.replacement;
        return url.toString();
    } catch { return value; }
}

export async function sanitizeArtifactFile(file, options = {}) {
    const input = await readFile(file, 'utf8');
    let output;
    try { output = JSON.stringify(sanitizeArtifactValue(JSON.parse(input), options), null, 2); if (input.endsWith('\n')) output += '\n'; }
    catch { output = redactText(input, options); }
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
    if (typeof value === 'string') return matchesAny(key, config.secretKeys) ? config.replacement : redactText(value, config);
    if (value === null || typeof value !== 'object') return value;
    if (seen.has(value)) return '[Circular]';
    seen.add(value);
    if (Array.isArray(value)) { const out = value.map((item) => sanitizeValue(item, config, seen, key)); seen.delete(value); return out; }
    const out = {};
    for (const [entryKey, entryValue] of Object.entries(value)) out[entryKey] = matchesAny(entryKey, config.secretKeys) ? config.replacement : sanitizeValue(entryValue, config, seen, entryKey);
    seen.delete(value);
    return out;
}

function normalizeRedactionOptions(options) {
    const base = PROFILES[options.profile || 'web'] || PROFILES.web;
    return { replacement: options.replacement || REDACTED, secretKeys: [...base.secretKeys, ...(options.secretKeys || [])], querySecretKeys: [...base.querySecretKeys, ...(options.querySecretKeys || [])] };
}

function matchesAny(key, patterns) {
    return Boolean(key) && patterns.some((pattern) => pattern instanceof RegExp ? pattern.test(key) : pattern === key);
}
