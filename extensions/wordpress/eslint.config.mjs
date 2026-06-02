/**
 * WordPress dependencies
 */
import wordpress from '@wordpress/eslint-plugin';

export default [
  {
    ignores: [
      'node_extensions/',
      'vendor/',
      'build/',
      'dist/',
      '*.min.js',
      'tests/',
    ],
  },
  ...wordpress.configs.recommended,
  {
    languageOptions: {
      globals: {
        wp: 'readonly',
        jQuery: 'readonly',
        ajaxurl: 'readonly',
      },
    },
    settings: {
      'import/resolver': {
        typescript: {
          alwaysTryTypes: true,
          project: [ '**/jsconfig.json', '**/tsconfig.json' ],
        },
      },
    },
    rules: {
      'prettier/prettier': 'off',
      '@wordpress/dependency-group': 'error',
      '@wordpress/i18n-translator-comments': 'warn',
      '@wordpress/no-unsafe-wp-apis': 'warn',
      'import/no-extraneous-dependencies': 'off',
      'no-console': 'warn',
      eqeqeq: [ 'error', 'always', { null: 'ignore' } ],
    },
  },
];
