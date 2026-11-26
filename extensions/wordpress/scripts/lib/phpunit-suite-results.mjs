import { readFile, writeFile } from 'node:fs/promises';

// A runner invocation owns several suites, but core consumes one sidecar.
// Never use that shared file as an individual suite's parser scratch space.
export function createPhpunitSuiteResults(destination) {
  const totals = { total: 0, passed: 0, failed: 0, skipped: 0 };
  const partials = new Set();
  return async (summary) => {
    if (!destination) return;
    for (const key of Object.keys(totals)) {
      const value = summary?.[key] ?? 0;
      if (!Number.isSafeInteger(value) || value < 0) {
        throw new Error(`Invalid PHPUnit suite count: ${key}`);
      }
      totals[key] += value;
    }
    if (summary?.partial) partials.add(summary.partial);
    const result = partials.size ? { ...totals, partial: [...partials].join(',') } : totals;
    await writeFile(destination, `${JSON.stringify(result)}\n`);
  };
}

export async function readPhpunitSuiteResults(file) {
  return JSON.parse(await readFile(file, 'utf8'));
}
