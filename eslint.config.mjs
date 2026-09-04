// Flat config. One config for the whole workspace: per-package copies drift, and a
// rule that is only enforced in some packages is a rule the reviewer cannot rely on.
import js from '@eslint/js';
import globals from 'globals';
import reactHooks from 'eslint-plugin-react-hooks';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ignores: [
      '**/dist/**',
      '**/node_modules/**',
      '**/.next/**',
      '**/target/**',
      'coverage/**',
      // Scratch space and a local database. Neither is in any tsconfig, so the typed
      // rules cannot run over them anyway — they would only produce parser errors.
      '.tmp/**',
      '.pgdata/**',
      // Generated: the generator owns this file, and `make protocol-check` owns
      // whether it is current. Linting it would only produce findings nobody may fix.
      'packages/protocol/src/generated.ts',
      // Next.js generates and owns these: the ambient types file it rewrites on every
      // build, and the static asset directory (service worker, icons) that sits outside
      // any tsconfig. Both only produce parser errors under the typed rules.
      'clients/web/next-env.d.ts',
      'clients/web/public/**',
      // The static export. `next build` writes it, `make build-web` produces it, and the
      // release workflow tars it verbatim — it is minified output of code that was already
      // linted as source, so linting it again can only produce findings in generated
      // chunks nobody may edit.
      'clients/web/out/**',
      // Design reference mockups (docs/design/*.tsx): standalone React+Tailwind demos that
      // document the look, not code that ships. They sit outside every tsconfig, so the
      // typed rules can only produce parser errors over them.
      'docs/design/**',
      // A user-authored design reference kept at the root, outside every tsconfig (the same
      // reason as docs/design/**). Not tracked, not shipped; linting it only errors.
      'new-ui-02.tsx',
      // Vendored dependencies: the Foundry submodules under contracts/lib (OpenZeppelin,
      // forge-std). Pinned upstream sources, not ours to lint — the same standing as
      // node_modules, just checked in so CI can build without fetching.
      'contracts/lib/**',
      // What Foundry writes when it builds and broadcasts: artifacts and run records, no
      // hand-edited source anywhere among them.
      'contracts/out/**',
      'contracts/cache/**',
      'contracts/broadcast/**',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
      globals: { ...globals.node },
    },
    rules: {
      // A rejected promise nobody awaited is a crash in Node and a silent no-op in a
      // browser. Both are worse than a compile error.
      //
      // `test()` from `node:test` is the one exception, and it is declared here rather than
      // by switching the rule off in test files. The runner owns the promise it returns;
      // awaiting it at the top level would serialise every case for no benefit, and `void`
      // at each of fifty call sites is noise that trains the eye to skip exactly the marker
      // this rule exists to make visible. Everything else inside a test — a `decodeFrame`
      // that was never awaited, an `inflateRaw` whose rejection would make the assertions
      // below it vacuous — is still an error.
      '@typescript-eslint/no-floating-promises': [
        'error',
        {
          allowForKnownSafeCalls: [
            {
              from: 'package',
              package: 'node:test',
              name: ['test', 'it', 'describe', 'before', 'after'],
            },
          ],
        },
      ],
      '@typescript-eslint/no-misused-promises': 'error',

      // `any` defeats the reason this codebase is in TypeScript. Vector runners parse
      // untyped JSON, so they narrow explicitly instead of casting.
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/no-unsafe-assignment': 'error',
      '@typescript-eslint/no-unsafe-member-access': 'error',

      // An unused parameter named `_` is deliberate; anything else is a leftover.
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],

      // Bit twiddling on `number` in a binary codec is not a smell, it is the job.
      'no-bitwise': 'off',
    },
  },
  {
    // The web client is the workspace's only React surface. Register the hooks plugin
    // here so its two rules are defined only where they apply; every other package is
    // plain TypeScript and would pay the parse cost for nothing. Browser globals join the
    // node globals from the base config (flat config merges languageOptions across
    // matching blocks) for code that touches the DOM, IndexedDB, and the service worker.
    files: ['clients/web/**/*.{ts,tsx}'],
    plugins: { 'react-hooks': reactHooks },
    languageOptions: { globals: { ...globals.browser } },
    rules: {
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'error',
    },
  },
  {
    // Config and tool files are plain scripts outside any package's tsconfig. The web
    // client's own top-level configs (next.config.mjs, postcss.config.mjs) are the same:
    // ESM the framework loads directly, not part of the typed project.
    files: ['*.mjs', '**/tools/**/*.mjs', 'clients/web/*.mjs'],
    ...tseslint.configs.disableTypeChecked,
  },
);
