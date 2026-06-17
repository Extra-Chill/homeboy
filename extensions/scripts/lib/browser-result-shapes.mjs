import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const shapes = require('./browser-result-shapes.cjs');

export const {
    BROWSER_RESULT_SCHEMA_VERSION,
    HOMEBOY_BENCH_RESULTS_SCHEMA,
    HOMEBOY_BROWSER_EVIDENCE_SCHEMA,
    buildBenchResultsEnvelope,
    buildBenchScenarioResult,
    buildBrowserBenchResult,
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
