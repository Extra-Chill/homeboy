import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const shapes = require('./browser-result-shapes.cjs');

export const {
    BROWSER_RESULT_SCHEMA_VERSION,
    collectBrowserPhases,
    normalizeBrowserArtifact,
    normalizeBrowserBottleneck,
    normalizeBrowserNetworkRequest,
    normalizeBrowserPerformanceProfile,
    normalizeBrowserPhaseMark,
    normalizeBrowserProfileTimings,
    normalizeBrowserTiming,
    normalizeTraceAssertion,
    normalizeTraceEnvelope,
    normalizeTraceEvent,
    stableJson,
} = shapes;

export default shapes;
