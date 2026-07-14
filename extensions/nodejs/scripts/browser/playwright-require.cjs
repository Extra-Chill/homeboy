const Module = require('node:module');
const path = require('node:path');

const runtimePackageDir = process.env.HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_PACKAGE_DIR;
const runtimeRequire = runtimePackageDir
    ? Module.createRequire(path.join(runtimePackageDir, 'package.json'))
    : null;
const resolveFilename = Module._resolveFilename;
const runtimeParent = runtimePackageDir ? new Module(path.join(runtimePackageDir, 'package.json')) : null;
if (runtimeParent) {
    runtimeParent.filename = path.join(runtimePackageDir, 'package.json');
    runtimeParent.paths = Module._nodeModulePaths(runtimePackageDir);
}

Module._resolveFilename = function homeboyPlaywrightResolve(request, parent, isMain, options) {
    if (runtimeRequire && (request === 'playwright' || request.startsWith('playwright/'))) {
        return resolveFilename.call(this, request, runtimeParent, false, options);
    }
    try {
        return resolveFilename.call(this, request, parent, isMain, options);
    } catch (error) {
        if (!runtimeRequire || (request !== 'playwright' && !request.startsWith('playwright/'))) throw error;
        return runtimeRequire.resolve(request);
    }
};
