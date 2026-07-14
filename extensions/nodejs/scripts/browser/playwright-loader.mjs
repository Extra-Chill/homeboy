import { pathToFileURL } from 'node:url';

const runtimePackageDir = process.env.HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_PACKAGE_DIR;

export async function resolve(specifier, context, nextResolve) {
    if (specifier !== 'playwright' && !specifier.startsWith('playwright/')) {
        return nextResolve(specifier, context);
    }

    if (!runtimePackageDir) throw new Error('HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_PACKAGE_DIR is required.');
    if (specifier === 'playwright') {
        // Playwright's CommonJS entrypoint assigns some exports dynamically.
        // A facade preserves the documented ESM named exports for consumers.
        return { url: new URL('./playwright-esm-facade.mjs', import.meta.url).href, shortCircuit: true };
    }
    // Resolve subpaths through Node's package resolver using the pinned runtime
    // package as the parent rather than returning a raw CommonJS file URL.
    return nextResolve(specifier, {
        ...context,
        parentURL: pathToFileURL(`${runtimePackageDir}/package.json`).href,
    });
}
