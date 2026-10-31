import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, relative, resolve } from 'node:path';

export const artifactDir = resolve(process.env.HOMEBOY_TRACE_ARTIFACT_DIR || '.');

export function artifactPath(name) {
    if (!name || typeof name !== 'string') {
        throw new Error('artifactPath requires a non-empty string name');
    }

    if (isAbsolute(name)) {
        return name;
    }

    return resolve(artifactDir, name);
}

export function artifactRelativePath(path) {
    const absolute = resolve(path);
    const rel = relative(artifactDir, absolute);
    return rel && !rel.startsWith('..') && !isAbsolute(rel) ? rel : absolute;
}

export async function writeArtifact(name, content) {
    const path = artifactPath(name);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, content);
    return {
        path,
        relativePath: artifactRelativePath(path),
    };
}
