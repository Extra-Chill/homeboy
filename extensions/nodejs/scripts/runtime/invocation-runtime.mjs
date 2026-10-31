import { mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const PORT_MIN = 1;
const PORT_MAX = 65535;

export function resolveHomeboyInvocationRuntime(options = {}) {
    const sourceEnv = options.env || process.env;
    const namespace = sanitizeSegment(options.namespace || 'workload');
    const invocationId = nonEmptyString(sourceEnv.HOMEBOY_INVOCATION_ID);
    const baseDirs = {
        state: nonEmptyString(sourceEnv.HOMEBOY_INVOCATION_STATE_DIR),
        artifact: nonEmptyString(sourceEnv.HOMEBOY_INVOCATION_ARTIFACT_DIR),
        tmp: nonEmptyString(sourceEnv.HOMEBOY_INVOCATION_TMP_DIR),
    };
    const portRange = parsePortRange(sourceEnv);
    const isolated = Boolean(invocationId || baseDirs.state || baseDirs.artifact || baseDirs.tmp);
    const dirs = isolated ? scopedDirs(baseDirs, namespace) : emptyDirs();
    const runtimeEnv = buildRuntimeEnv({ sourceEnv, invocationId, namespace, dirs, portRange, isolated });

    return {
        isolated,
        namespace,
        invocationId,
        baseDirs,
        dirs,
        portRange,
        env: runtimeEnv,
        childEnv(extraEnv = {}) {
            return { ...runtimeEnv, ...extraEnv };
        },
        async prepareDirs() {
            await Promise.all(Object.values(dirs).filter(Boolean).map((dir) => mkdir(dir, { recursive: true })));
            return dirs;
        },
        assertPort(port) {
            if (!portRange) return Number(port);
            const normalizedPort = normalizePort(port, 'port');
            if (normalizedPort < portRange.base || normalizedPort > portRange.max) {
                throw new Error(`Port ${normalizedPort} is outside Homeboy invocation range ${portRange.base}-${portRange.max}.`);
            }
            return normalizedPort;
        },
    };
}

function scopedDirs(baseDirs, namespace) {
    const state = scopedDir(baseDirs.state, namespace);
    const artifact = scopedDir(baseDirs.artifact, namespace);
    const tmp = scopedDir(baseDirs.tmp, namespace);

    return {
        state,
        artifact,
        tmp,
        home: state ? join(state, 'home') : null,
        config: state ? join(state, 'config') : null,
        cache: state ? join(state, 'cache') : null,
        data: state ? join(state, 'data') : null,
    };
}

function emptyDirs() {
    return {
        state: null,
        artifact: null,
        tmp: null,
        home: null,
        config: null,
        cache: null,
        data: null,
    };
}

function scopedDir(baseDir, namespace) {
    return baseDir ? resolve(baseDir, namespace) : null;
}

function buildRuntimeEnv({ sourceEnv, invocationId, namespace, dirs, portRange, isolated }) {
    const env = { ...sourceEnv };

    if (isolated) {
        env.HOMEBOY_INVOCATION_NAMESPACE = namespace;
        if (invocationId) env.HOMEBOY_INVOCATION_ID = invocationId;
        if (dirs.state) env.HOMEBOY_INVOCATION_STATE_DIR = dirs.state;
        if (dirs.artifact) env.HOMEBOY_INVOCATION_ARTIFACT_DIR = dirs.artifact;
        if (dirs.tmp) {
            env.HOMEBOY_INVOCATION_TMP_DIR = dirs.tmp;
            env.TMPDIR = dirs.tmp;
            env.TMP = dirs.tmp;
            env.TEMP = dirs.tmp;
        }
        if (dirs.home) env.HOME = dirs.home;
        if (dirs.config) env.XDG_CONFIG_HOME = dirs.config;
        if (dirs.cache) env.XDG_CACHE_HOME = dirs.cache;
        if (dirs.data) env.XDG_DATA_HOME = dirs.data;
        if (dirs.state) env.XDG_STATE_HOME = dirs.state;
    }

    if (portRange) {
        env.HOMEBOY_INVOCATION_PORT_BASE = String(portRange.base);
        env.HOMEBOY_INVOCATION_PORT_MAX = String(portRange.max);
    }

    return env;
}

function parsePortRange(env) {
    const rawBase = nonEmptyString(env.HOMEBOY_INVOCATION_PORT_BASE);
    const rawMax = nonEmptyString(env.HOMEBOY_INVOCATION_PORT_MAX);

    if (!rawBase && !rawMax) return null;
    if (!rawBase || !rawMax) {
        throw new Error('HOMEBOY_INVOCATION_PORT_BASE and HOMEBOY_INVOCATION_PORT_MAX must be set together.');
    }

    const base = normalizePort(rawBase, 'HOMEBOY_INVOCATION_PORT_BASE');
    const max = normalizePort(rawMax, 'HOMEBOY_INVOCATION_PORT_MAX');
    if (base > max) {
        throw new Error(`HOMEBOY_INVOCATION_PORT_BASE (${base}) must be less than or equal to HOMEBOY_INVOCATION_PORT_MAX (${max}).`);
    }

    return { base, max };
}

function normalizePort(value, label) {
    const raw = String(value ?? '').trim();
    if (!/^\d+$/.test(raw)) {
        throw new Error(`${label} must be an integer port, got "${raw}".`);
    }

    const port = Number(raw);
    if (!Number.isSafeInteger(port) || port < PORT_MIN || port > PORT_MAX) {
        throw new Error(`${label} must be between ${PORT_MIN} and ${PORT_MAX}, got ${raw}.`);
    }

    return port;
}

function nonEmptyString(value) {
    if (typeof value !== 'string') return null;
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : null;
}

function sanitizeSegment(value) {
    const segment = String(value || 'workload')
        .trim()
        .replace(/[^A-Za-z0-9._-]+/g, '-')
        .replace(/^-+|-+$/g, '');
    return segment || 'workload';
}
