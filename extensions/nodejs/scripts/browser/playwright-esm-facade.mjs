import { createRequire } from 'node:module';
import { join } from 'node:path';

const runtimePackageDir = process.env.HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_PACKAGE_DIR;
if (!runtimePackageDir) throw new Error('HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_PACKAGE_DIR is required.');

const playwright = createRequire(join(runtimePackageDir, 'package.json'))('playwright');

export default playwright;
export const chromium = playwright.chromium;
export const firefox = playwright.firefox;
export const webkit = playwright.webkit;
export const _electron = playwright._electron;
export const _android = playwright._android;
export const devices = playwright.devices;
export const selectors = playwright.selectors;
export const request = playwright.request;
export const errors = playwright.errors;
