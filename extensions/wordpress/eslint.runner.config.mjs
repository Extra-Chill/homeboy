/**
 * External dependencies
 */
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { createJiti } from 'jiti';

/**
 * Internal dependencies
 */
import defaults from './eslint.config.mjs';

const componentPath = process.env.HOMEBOY_ESLINT_COMPONENT_PATH;
const configNames = [
  'eslint.config.js',
  'eslint.config.mjs',
  'eslint.config.cjs',
  'eslint.config.ts',
  'eslint.config.mts',
  'eslint.config.cts',
];

let localConfig = [];

if ( componentPath ) {
  const localConfigPath = configNames
    .map( ( configName ) => resolve( componentPath, configName ) )
    .find( ( configPath ) => existsSync( configPath ) );

  if ( localConfigPath ) {
    const jiti = createJiti( import.meta.url );
    const exportedConfig = await jiti.import( localConfigPath, {
      default: true,
    } );
    localConfig = Array.isArray( exportedConfig )
      ? exportedConfig
      : [ exportedConfig ];
  }
}

export default [ ...defaults, ...localConfig ];
