import { register } from 'node:module';

register(new URL('./playwright-loader.mjs', import.meta.url), import.meta.url);
