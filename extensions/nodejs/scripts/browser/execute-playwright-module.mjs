import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const [modulePath, ...args] = process.argv.slice(2);
if (!modulePath) throw new Error('A module path is required.');

process.argv = [process.argv[0], modulePath, ...args];
await import(pathToFileURL(resolve(modulePath)).href);
