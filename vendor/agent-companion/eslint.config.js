import js from '@eslint/js';
import reactHooks from 'eslint-plugin-react-hooks';
import tseslint from 'typescript-eslint';

// The migration adds TypeScript and React incrementally. Lint covers the
// migrated frontend only; un-migrated view code stays JS and joins this scope
// when its phase converts it. `npm run lint` must stay green at every stage.
export default tseslint.config(
  {
    ignores: [
      '**/*.js',
      '**/*.mjs',
      '**/*.cjs',
      'dist/**',
      'node_modules/**',
      'artifacts/**',
      'target/**',
      'src-tauri/**',
      'crates/**',
      'collector/**',
      'scripts/**',
      'tests/**',
    ],
  },
  {files: ['src/**/*.ts', 'src/**/*.tsx'], ...js.configs.recommended},
  ...tseslint.configs.recommended,
  {
    files: ['src/**/*.ts', 'src/**/*.tsx'],
    ...reactHooks.configs.flat.recommended,
    rules: {
      // The bridge and the models deliberately swallow storage/import failures:
      // an unavailable cache must not stop monitoring.
      'no-empty': ['error', {allowEmptyCatch: true}],
      '@typescript-eslint/no-unused-vars': ['error', {argsIgnorePattern: '^_', varsIgnorePattern: '^_'}],
    },
  },
);
