import { mkdir, writeFile } from 'node:fs/promises';
import { basename, dirname } from 'node:path';

// R-7 percentile, the runner contract used by Homeboy BenchResults producers.
export function homeboyBenchPercentile(sortedValues, p) {
    const n = sortedValues.length;
    if (n === 0) return 0;
    if (n === 1) return sortedValues[0];
    const rank = p * (n - 1);
    const lo = Math.floor(rank);
    const hi = Math.ceil(rank);
    if (lo === hi) return sortedValues[lo];
    const frac = rank - lo;
    return sortedValues[lo] * (1 - frac) + sortedValues[hi] * frac;
}

// scenario slug helper: turn a workload basename into a stable BenchScenario id.
export function homeboyBenchScenarioId(file, extensionPattern = /\.[^.]+$/) {
    return basename(file)
        .replace(extensionPattern, '')
        .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '');
}

export function homeboyBenchResultsEnvelope(componentId, iterations, scenarios) {
    return {
        component_id: componentId,
        iterations,
        scenarios,
    };
}

export async function homeboyWriteBenchResults(resultsFile, componentId, iterations, scenarios) {
    await mkdir(dirname(resultsFile), { recursive: true });
    await writeFile(
        resultsFile,
        JSON.stringify(homeboyBenchResultsEnvelope(componentId, iterations, scenarios), null, 2)
    );
}

export async function homeboyWriteEmptyBenchResults(resultsFile, componentId, iterations = 0) {
    await homeboyWriteBenchResults(resultsFile, componentId, iterations, []);
}
