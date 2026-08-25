// @ts-check

import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

// The repository root, two levels above clients/web. `output: 'standalone'` traces its
// runtime file set relative to a root it otherwise infers from lockfile location; that
// inference is ambiguous in a monorepo and decides whether the emitted server.js lands
// under clients/web/ or at the bundle root. Pinning it makes the container's run path
// (`node clients/web/server.js`) deterministic across machines.
const workspaceRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

/**
 * Next.js configuration for the Migo web client.
 *
 * The SDK and its sibling packages ship as compiled ESM in a pnpm workspace; `transpilePackages`
 * lets Next resolve and bundle them through its own pipeline so their `.js` import specifiers and
 * workspace symlinks are handled uniformly. `output: 'standalone'` produces a self-contained server
 * bundle that the container image under `infra/` can run without the full node_modules tree.
 *
 * ESLint runs from the repo root (a single flat config governs every package), so Next's built-in
 * lint-on-build is disabled here to avoid a second, conflicting configuration. Type errors still fail
 * the build.
 *
 * The `webpack` hook re-asserts `.js` -> `.ts`/`.tsx` extension resolution. This project imports with
 * explicit `.js` specifiers (matching the workspace's NodeNext libraries), but Next's default extension
 * aliasing is not re-applied to requests produced by the tsconfig `@/*` path alias, which otherwise
 * leaves `@/.../x.js` imports unresolved at build time. Declaring it here covers both cases uniformly.
 */

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  output: 'standalone',
  outputFileTracingRoot: workspaceRoot,
  transpilePackages: ['@migo/sdk', '@migo/protocol', '@migo/wire', '@migo/crypto'],
  eslint: { ignoreDuringBuilds: true },
  typescript: { ignoreBuildErrors: false },
  webpack: (config) => {
    config.resolve.extensionAlias = {
      ...config.resolve.extensionAlias,
      '.js': ['.ts', '.tsx', '.js'],
      '.jsx': ['.tsx', '.jsx'],
      '.mjs': ['.mts', '.mjs'],
      '.cjs': ['.cts', '.cjs'],
    };
    return config;
  },
};

export default nextConfig;
