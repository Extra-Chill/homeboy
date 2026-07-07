#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

DEV_EXTENSION_ROOT="$TMP_DIR/dev-extensions/nodejs"
SNAPSHOT_DIR="$DEV_EXTENSION_ROOT/c976416565de9af3"

mkdir -p "$DEV_EXTENSION_ROOT/scripts/lib" "$SNAPSHOT_DIR/scripts/bench"
cp "$ROOT_DIR/scripts/lib/browser-result-shapes.mjs" "$DEV_EXTENSION_ROOT/scripts/lib/browser-result-shapes.mjs"
cp "$ROOT_DIR/scripts/lib/browser-result-shapes.cjs" "$DEV_EXTENSION_ROOT/scripts/lib/browser-result-shapes.cjs"
cp "$ROOT_DIR/scripts/bench/bench-runner.mjs" "$SNAPSHOT_DIR/scripts/bench/bench-runner.mjs"

node --input-type=module - "$SNAPSHOT_DIR/scripts/bench/bench-runner.mjs" <<'NODE'
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
NODE
