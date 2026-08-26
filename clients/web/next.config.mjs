// @ts-check

/**
 * Next.js configuration for the Migo web client.
 *
 * # A static bundle, not a server
 *
 * `output: 'export'` emits plain HTML, CSS and JavaScript into `out/`. Nothing in this client needs a
 * server: every byte of message content is encrypted and decrypted in the browser, keys live in
 * IndexedDB on the device, and the only network peers are the REST API and the gateway WebSocket. A
 * Node process rendering pages would sit between the user and their own keys for no benefit, and it
 * would be one more place a plaintext could accidentally exist. There is nothing to render server-side
 * because the server cannot read anything.
 *
 * That also makes the deployment story honest: the artifact is a directory of files. It can be served
 * by any static host, a CDN, or the small `tools/serve.mjs` in this package, and every one of them
 * serves identical bytes. `client_web-<version>.tar.gz` in a release is that directory.
 *
 * Because there is no server, there is no dynamic route: `/chat/[id]` cannot be prerendered, since
 * conversation ids only exist at runtime on the device. The open conversation lives in the URL
 * fragment instead (`/chat#c=<id>`), which a static host never receives — see
 * `src/lib/migo/use-open-conversation.ts`.
 *
 * # The rest
 *
 * `trailingSlash: true` makes every route a directory with an `index.html`, which is what plain file
 * servers and object stores resolve without special rules.
 *
 * `images.unoptimized` is required by the export: the default image loader needs a running server.
 *
 * The SDK and its sibling packages ship as compiled ESM in a pnpm workspace; `transpilePackages` lets
 * Next resolve and bundle them through its own pipeline so their `.js` import specifiers and workspace
 * symlinks are handled uniformly.
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
  output: 'export',
  trailingSlash: true,
  images: { unoptimized: true },
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
