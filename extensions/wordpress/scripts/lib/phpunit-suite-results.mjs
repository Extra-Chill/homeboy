import { readFile, writeFile } from 'node:fs/promises';

// A runner invocation owns several suites, but core consumes one sidecar.
// Never use that shared file as an individual suite's parser scratch space.
export function createPhpunitSuiteResults(destination) {
  const totals = { total: 0, passed: 0, failed: 0, skipped: 0 };
  const partials = new Set();
  const metadata = {};
  return async (summary) => {
    if (!destination) return;
    for (const key of Object.keys(totals)) {
      const value = summary?.[key] ?? 0;
      if (!Number.isSafeInteger(value) || value < 0) {
        throw new Error(`Invalid PHPUnit suite count: ${key}`);
      }
      totals[key] += value;
    }
    // Runtime writers may attach provenance or other contract fields. Keep
    // those fields even when a later host suite supplies only numeric counts.
    for (const [key, value] of Object.entries(summary ?? {})) {
      if (!(key in totals) && key !== 'partial') metadata[key] = value;
    }
    if (summary?.partial) partials.add(summary.partial);
    const result = { ...metadata, ...totals };
    if (partials.size) result.partial = [...partials].join(',');
    await writeFile(destination, `${JSON.stringify(result, null, 2)}\n`);
  };
}

export async function readPhpunitSuiteResults(file) {
  return JSON.parse(await readFile(file, 'utf8'));
}
