#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

DEV_EXTENSION_ROOT="$TMP_DIR/dev-extensions/nodejs"
SNAPSHOT_DIR="$DEV_EXTENSION_ROOT/c976416565de9af3"

mkdir -p "$DEV_EXTENSION_ROOT"
cp -R "$ROOT_DIR/." "$SNAPSHOT_DIR"
test ! -e "$DEV_EXTENSION_ROOT/scripts/lib/browser-result-shapes.mjs"

node --input-type=module - "$SNAPSHOT_DIR/scripts/bench/bench-runner.mjs" "$SNAPSHOT_DIR" <<'NODE'
import { readFile } from 'node:fs/promises';

const runner = await readFile(process.argv[2], 'utf8');
const importPath = runner.match(/from ['"]([^'"]*browser-result-shapes\.mjs)['"]/)?.[1];
if (!importPath) {
    throw new Error('bench-runner.mjs does not import browser-result-shapes.mjs');
}

const moduleUrl = new URL(importPath, `file://${process.argv[2]}`);
const shapes = await import(moduleUrl.href);

if (typeof shapes.normalizeBrowserBenchWorkloadResult !== 'function') {
    throw new Error('normalizeBrowserBenchWorkloadResult export missing');
}
if (typeof shapes.normalizeBenchArtifact !== 'function') {
    throw new Error('normalizeBenchArtifact export missing');
}

const normalized = shapes.normalizeBrowserBenchWorkloadResult({
    browser_profile: { page_url: 'https://example.test/' },
});
if (normalized.browser_profile?.page_url !== 'https://example.test/') {
    throw new Error('browser result normalization returned an invalid shape');
}

for (const relativeModule of [
    'scripts/bench/browser-helper.mjs',
    'scripts/trace/lib/browser-waterfall.mjs',
    'scripts/trace/lib/timeline.mjs',
]) {
    await import(new URL(relativeModule, `file://${process.argv[3]}/`).href);
}
NODE
