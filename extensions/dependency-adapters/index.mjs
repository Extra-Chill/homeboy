import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const adapterRoot = path.dirname(fileURLToPath(import.meta.url));
const indexPath = path.join(adapterRoot, 'index.json');

export function dependencyAdapterManifestIndex() {
	return JSON.parse(fs.readFileSync(indexPath, 'utf8'));
}

export function dependencyAdapterManifestPaths() {
	return dependencyAdapterManifestIndex().manifests.map((manifest) => path.join(adapterRoot, manifest.path));
}

export function loadDependencyAdapterManifests() {
	return dependencyAdapterManifestPaths().map((manifestPath) => JSON.parse(fs.readFileSync(manifestPath, 'utf8')));
}
