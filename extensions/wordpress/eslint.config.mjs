/**
 * WordPress dependencies
 */
import wordpress from '@wordpress/eslint-plugin';

export default [
  {
    ignores: [
      'node_modules/',
      '**/vendor/**',
      '**/vendor_prefixed/**',
      '**/vendor-prefixed/**',
      '**/vendor_scoped/**',
      '**/vendor-scoped/**',
      '**/build/**',
      '**/dist/**',
      '*.min.js',
      'tests/',
    ],
  },
  {
    files: [ '**/*.{js,jsx,ts,tsx}' ],
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
      /*
       * WordPress script-handle packages (`@wordpress/*`, `jquery`, `lodash`,
       * `react`, `react-dom`, `moment`) are provided at runtime by WordPress via
       * `@wordpress/scripts`' webpack externals — they are deliberately NOT
       * installed into a block plugin's node_modules. Without this list,
       * `import/no-unresolved` false-flags every `import ... from '@wordpress/*'`
       * in every block plugin we lint, which cannot be fixed from the plugin
       * side (the packages are never on disk). Treat them as resolvable
       * core modules so the rule only fires on genuinely unresolvable imports.
       */
      'import/core-modules': [
        '@wordpress/a11y',
        '@wordpress/annotations',
        '@wordpress/api-fetch',
        '@wordpress/autop',
        '@wordpress/blob',
        '@wordpress/block-directory',
        '@wordpress/block-editor',
        '@wordpress/block-library',
        '@wordpress/block-serialization-default-parser',
        '@wordpress/blocks',
        '@wordpress/components',
        '@wordpress/compose',
        '@wordpress/core-data',
        '@wordpress/data',
        '@wordpress/data-controls',
        '@wordpress/date',
        '@wordpress/deprecated',
        '@wordpress/dom',
        '@wordpress/dom-ready',
        '@wordpress/edit-post',
        '@wordpress/editor',
        '@wordpress/element',
        '@wordpress/escape-html',
        '@wordpress/hooks',
        '@wordpress/html-entities',
        '@wordpress/i18n',
        '@wordpress/icons',
        '@wordpress/keyboard-shortcuts',
        '@wordpress/keycodes',
        '@wordpress/media-utils',
        '@wordpress/notices',
        '@wordpress/plugins',
        '@wordpress/primitives',
        '@wordpress/priority-queue',
        '@wordpress/private-apis',
        '@wordpress/reusable-blocks',
        '@wordpress/rich-text',
        '@wordpress/server-side-render',
        '@wordpress/url',
        '@wordpress/viewport',
        '@wordpress/warning',
        '@wordpress/wordcount',
        'jquery',
        'lodash',
        'moment',
        'react',
        'react-dom',
      ],
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
