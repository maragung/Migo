#!/usr/bin/env node
// Serves the exported web client over plain HTTP.
//
// The client is a static bundle (see next.config.mjs): `next build` writes `out/` and that directory is
// the whole artifact. This script exists so the artifact can be run with nothing but Node — no nginx,
// no Next runtime, no dependencies outside the standard library. It is what the container image runs
// and what `pnpm --filter @migo/web start` runs, so a developer and production serve the same bytes
// through the same code path.
//
// It is deliberately a file server and nothing else. It has no route table, no proxy, no API surface,
// and it never reads a request body. There is no server-side state to attack because there is no
// server-side state: every byte it can return is already public, sitting in `out/`.
//
// Usage: node tools/serve.mjs [--port 19992] [--host 0.0.0.0] [--dir out]
//        MIGO_WEB_PORT / PORT, MIGO_WEB_HOST / HOST, MIGO_WEB_DIR override the defaults.

import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readdir, readFile, stat } from 'node:fs/promises';
import { createServer } from 'node:http';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The port Migo's web client is served on. */
const DEFAULT_PORT = 19992;

/** Bind on every interface: inside a container, localhost would be unreachable from outside it. */
const DEFAULT_HOST = '0.0.0.0';

/** Where `next build` puts the export, relative to this package. */
const DEFAULT_DIR = 'out';

const packageRoot = resolve(fileURLToPath(new URL('..', import.meta.url)));

/**
 * Content types for everything the export can contain.
 *
 * An explicit table rather than a dependency: the set of extensions a static Next export emits is
 * small and known, and a wrong `Content-Type` on a script is a page that silently does not run.
 */
const CONTENT_TYPES = new Map(
  Object.entries({
    '.html': 'text/html; charset=utf-8',
    '.js': 'text/javascript; charset=utf-8',
    '.mjs': 'text/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.json': 'application/json; charset=utf-8',
    '.webmanifest': 'application/manifest+json; charset=utf-8',
    '.map': 'application/json; charset=utf-8',
    '.txt': 'text/plain; charset=utf-8',
    '.svg': 'image/svg+xml',
    '.png': 'image/png',
    '.jpg': 'image/jpeg',
    '.jpeg': 'image/jpeg',
    '.webp': 'image/webp',
    '.avif': 'image/avif',
    '.gif': 'image/gif',
    '.ico': 'image/x-icon',
    '.woff': 'font/woff',
    '.woff2': 'font/woff2',
    '.ttf': 'font/ttf',
    '.wasm': 'application/wasm',
  }),
);

/**
 * The Content-Security-Policy the web client runs under (brief §57, §108, §164: "CSP bukan
 * pelengkap, tetapi bagian dari model keamanan E2E di web" — one XSS is enough to misuse even a
 * non-extractable key, so the policy is part of the E2E model, not a garnish).
 *
 * The same static directives are carried by the root layout's meta policy
 * (src/app/layout.tsx), which governs the bundle under any host that sends no header;
 * test/csp.test.ts compares the two so they cannot drift. What only this header can do is
 * supply the strict script policy — see `script-src` below — and `frame-ancestors`, which a
 * meta policy cannot carry. Directive by directive:
 *
 *   * `default-src 'self'` — the baseline: everything same-origin unless a directive below says
 *     otherwise, and nothing from a data: URL by accident.
 *   * `script-src 'self'` plus one `'sha256-…'` per inline script — the one exception the framework
 *     forces. Next's App Router embeds its hydration payload as inline
 *     `<script>self.__next_f.push(...)</script>` chunks (server/app-render/use-flight-response.js
 *     writes them) whose bytes differ per build, so they cannot be listed ahead of time. Each one
 *     is pinned by the SHA-256 of its exact bytes, computed at startup from the very directory
 *     this server hands out — the same strictness as 'self', with none of 'unsafe-inline'. The
 *     client's own code carries no inline script at all: the pre-paint theme restore is a
 *     same-origin file (public/theme-init.js) for exactly this reason. And there is no
 *     'unsafe-eval' anywhere, because nothing in the bundle evaluates strings.
 *   * `style-src 'self' 'unsafe-inline'` — the one concession, and an honest one: the components
 *     position context menus, presence dots and progress bars through `style={{}}` props, which
 *     the DOM turns into `style` attributes and CSP3 gates under style-src; React cannot move
 *     them to classes. It buys style placement, never script execution.
 *   * `img-src … http: https:` — media objects and avatars render from short-lived URLs on the
 *     API origin, which is user-configured (see connect-src); `blob:` and `data:` cover the
 *     client-side object URLs and the captcha challenge PNG.
 *   * `media-src 'self' blob:` — voice notes play back from object URLs; WebRTC streams use
 *     srcObject, which is not a fetch and needs no directive. STUN/TURN ICE candidates are not
 *     governed by CSP either — no shipped directive covers them.
 *   * `connect-src 'self' http: https: ws: wss:` — scheme-wide rather than origin-listed, and
 *     deliberately so: the REST and gateway origins are *user-configured* — a person points the
 *     client at their own server at sign-in, and that choice is persisted, not baked into the
 *     build. A strict CSP cannot enumerate origins that do not exist until a user types them;
 *     listing the one deployment this file happens to serve from would break every other one and
 *     teach nothing. Scheme-only is the honest ceiling for a connect surface whose destinations
 *     the user owns, and it is paired with the harder rule that no script other than the app's
 *     own ever gets to run to make a connection.
 *   * `worker-src 'self'` / `manifest-src 'self'` / `font-src 'self'` — the service worker, the
 *     PWA manifest, and a system font stack: all same-origin, no remote fonts.
 *   * `frame-src 'self'` — the store panel docks the same-origin /store/ bundle in an iframe.
 *   * `object-src 'none'`, `base-uri 'self'`, `form-action 'self'` — no plugins, no <base>
 *     hijack, and forms that never navigate anywhere their page did not already live.
 *   * `frame-ancestors 'none'` — header-only (a meta policy cannot carry it); it says the same
 *     thing as the X-Frame-Options below, in the language modern browsers honour first.
 */
const STATIC_DIRECTIVES = [
  "default-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' blob: data: http: https:",
  "media-src 'self' blob:",
  "connect-src 'self' http: https: ws: wss:",
  "font-src 'self'",
  "worker-src 'self'",
  "manifest-src 'self'",
  "frame-src 'self'",
  "object-src 'none'",
  "base-uri 'self'",
  "form-action 'self'",
  "frame-ancestors 'none'",
];

/**
 * Builds the policy string for the inline-script digests of the bundle being served.
 *
 * `script-src` sits immediately after `default-src`: the order states the argument — a same-origin
 * baseline, and the single, hash-scoped exception the framework's hydration payload forces onto it.
 */
export function buildContentSecurityPolicy(inlineScriptDigests = []) {
  const digests = [...new Set(inlineScriptDigests)].map((digest) => `'sha256-${digest}'`);
  const scriptSrc = `script-src 'self'${digests.length === 0 ? '' : ` ${digests.join(' ')}`}`;
  const [baseline, ...rest] = STATIC_DIRECTIVES;
  return [baseline, scriptSrc, ...rest].join('; ');
}

/** An inline script in served HTML: a `<script>` whose attributes do not include `src`. */
const INLINE_SCRIPT = /<script(?![^>]*\bsrc\s*=)[^>]*>([\s\S]*?)<\/script>/gi;

/** Every `.html` file under `dir`, sorted so the policy built from them is deterministic. */
async function collectHtmlFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const nested = await Promise.all(
    entries.map(async (entry) => {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        return collectHtmlFiles(path);
      }
      return entry.name.endsWith('.html') ? [path] : [];
    }),
  );
  return nested.flat().sort();
}

/**
 * The SHA-256 digests (base64) of every inline script in the bundle's HTML pages.
 *
 * The scan is deliberately simple — a regex over the files Next wrote, no HTML parser — because it
 * runs over this bundle's own generated output, where the only inline scripts are the framework's
 * hydration chunks (JSON-stringified with `<` escaped, so none of them contains a literal
 * `</script>` that could split a match). The browser hashes an inline script's exact text content;
 * the regex captures exactly the bytes between the tags, so the digests it yields are the digests
 * the policy will be checked against.
 */
export async function inlineScriptDigests(root) {
  const digests = [];
  for (const file of await collectHtmlFiles(root)) {
    const html = await readFile(file, 'utf8');
    for (const match of html.matchAll(INLINE_SCRIPT)) {
      digests.push(createHash('sha256').update(match[1], 'utf8').digest('base64'));
    }
  }
  return digests;
}

/** The full policy, once `main` has hashed the bundle; 'self'-only until then. */
let contentSecurityPolicy = buildContentSecurityPolicy();

/**
 * Security headers applied to every response.
 *
 * `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy` isolate the browsing context, so a
 * window this page opens (or that opens it) cannot reach into it. The rest are the ordinary hardening
 * a static host should send and cost nothing to get right here. The Content-Security-Policy is a
 * getter so every response carries the policy as it stood at the moment of sending: `main` computes
 * the full one — with this bundle's inline-script hashes — before the server starts listening.
 */
const SECURITY_HEADERS = {
  'X-Content-Type-Options': 'nosniff',
  'X-Frame-Options': 'DENY',
  'Referrer-Policy': 'no-referrer',
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
  'Permissions-Policy': 'geolocation=(), payment=(), usb=()',
  get 'Content-Security-Policy'() {
    return contentSecurityPolicy;
  },
};

/** Reads `--flag value` pairs, falling back to environment variables and then to the defaults. */
function readOptions(argv) {
  const flags = new Map();
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg.startsWith('--')) {
      const [name, inline] = arg.slice(2).split('=', 2);
      if (inline !== undefined) {
        flags.set(name, inline);
      } else {
        flags.set(name, argv[index + 1]);
        index += 1;
      }
    }
  }
  const port = Number(
    flags.get('port') ?? process.env.MIGO_WEB_PORT ?? process.env.PORT ?? DEFAULT_PORT,
  );
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`invalid port: ${flags.get('port') ?? process.env.PORT}`);
  }
  return {
    port,
    host: flags.get('host') ?? process.env.MIGO_WEB_HOST ?? process.env.HOST ?? DEFAULT_HOST,
    dir: resolve(packageRoot, flags.get('dir') ?? process.env.MIGO_WEB_DIR ?? DEFAULT_DIR),
  };
}

/**
 * Maps a request path to a file inside `root`, or null if it escapes.
 *
 * The traversal check is the one piece of security this server has to get right. Decoding first and
 * normalising after is what catches `%2e%2e%2f`; comparing the resolved path against `root + sep` is
 * what catches a symlink-free `../` that normalisation alone would leave in place. A path that does not
 * resolve inside `root` is refused rather than clamped, because a clamped traversal is a bug that looks
 * like it works.
 */
function resolveInside(root, requestPath) {
  let decoded;
  try {
    decoded = decodeURIComponent(requestPath);
  } catch {
    return null;
  }
  if (decoded.includes('\0')) {
    return null;
  }
  const candidate = resolve(join(root, normalize(decoded)));
  if (candidate !== root && !candidate.startsWith(root + sep)) {
    return null;
  }
  return candidate;
}

/** The file to serve for a resolved path: itself, its `index.html`, or its `.html` sibling. */
async function locate(candidate) {
  try {
    const info = await stat(candidate);
    if (info.isFile()) {
      return { path: candidate, size: info.size, mtime: info.mtimeMs };
    }
    if (info.isDirectory()) {
      // `trailingSlash: true` means every route is a directory holding an index.html.
      const index = join(candidate, 'index.html');
      const indexInfo = await stat(index);
      if (indexInfo.isFile()) {
        return { path: index, size: indexInfo.size, mtime: indexInfo.mtimeMs };
      }
    }
    return null;
  } catch {
    // A route requested without its trailing slash: `/chat` for `out/chat/index.html`. Next also emits
    // `chat.html` in some configurations, so try that too before giving up.
    try {
      const sibling = `${candidate}.html`;
      const info = await stat(sibling);
      if (info.isFile()) {
        return { path: sibling, size: info.size, mtime: info.mtimeMs };
      }
    } catch {
      return null;
    }
    return null;
  }
}

/**
 * Cache policy for a served file.
 *
 * Next fingerprints everything under `/_next/static/`, so those are immutable for a year: the filename
 * changes when the content does. Everything else — HTML above all — must be revalidated, or a
 * deployment leaves browsers running last week's bundle against this week's API.
 */
function cacheControl(urlPath, contentType) {
  if (urlPath.startsWith('/_next/static/')) {
    return 'public, max-age=31536000, immutable';
  }
  if (contentType.startsWith('text/html')) {
    return 'no-cache';
  }
  return 'public, max-age=0, must-revalidate';
}

/** Writes a bodyless error response. */
function fail(response, status, method) {
  response.writeHead(status, {
    ...SECURITY_HEADERS,
    'Content-Type': 'text/plain; charset=utf-8',
    'Content-Length': '0',
  });
  response.end();
  void method;
}

async function handle(request, response, root) {
  const method = request.method ?? 'GET';
  if (method !== 'GET' && method !== 'HEAD') {
    // A static file server has no other verbs. Answering 405 rather than 404 says so plainly.
    response.writeHead(405, { ...SECURITY_HEADERS, Allow: 'GET, HEAD', 'Content-Length': '0' });
    response.end();
    return;
  }

  // `new URL` with a fixed base parses the path and drops the query and fragment. A fragment never
  // arrives here anyway — browsers do not send it — which is exactly why the open conversation is kept
  // in one.
  const url = new URL(request.url ?? '/', 'http://localhost');
  const requestPath = url.pathname;

  // A liveness endpoint for the container healthcheck. It answers before any filesystem work, so it
  // stays true even if the bundle directory is missing, which is the failure it needs to report.
  if (requestPath === '/healthz') {
    const body = 'ok\n';
    response.writeHead(200, {
      ...SECURITY_HEADERS,
      'Content-Type': 'text/plain; charset=utf-8',
      'Content-Length': Buffer.byteLength(body),
      'Cache-Control': 'no-store',
    });
    response.end(method === 'HEAD' ? undefined : body);
    return;
  }

  const candidate = resolveInside(root, requestPath);
  if (candidate === null) {
    fail(response, 400, method);
    return;
  }

  let found = await locate(candidate);
  if (found === null) {
    // Unknown path: fall back to the app shell so client-side routing can render its own not-found
    // state. 404 on the status line, 200-worth of HTML in the body — the status is what a crawler and a
    // monitor read, and lying about it to make a page render would hide broken links.
    const shell = await locate(join(root, 'index.html'));
    if (shell === null) {
      fail(response, 404, method);
      return;
    }
    const body = createReadStream(shell.path);
    response.writeHead(404, {
      ...SECURITY_HEADERS,
      'Content-Type': 'text/html; charset=utf-8',
      'Content-Length': shell.size,
      'Cache-Control': 'no-cache',
    });
    if (method === 'HEAD') {
      body.destroy();
      response.end();
      return;
    }
    body.pipe(response);
    return;
  }

  const contentType =
    CONTENT_TYPES.get(extname(found.path).toLowerCase()) ?? 'application/octet-stream';
  const etag = `"${found.size.toString(16)}-${Math.floor(found.mtime).toString(16)}"`;
  const headers = {
    ...SECURITY_HEADERS,
    'Content-Type': contentType,
    'Content-Length': found.size,
    'Cache-Control': cacheControl(requestPath, contentType),
    ETag: etag,
  };

  if (request.headers['if-none-match'] === etag) {
    response.writeHead(304, {
      ...SECURITY_HEADERS,
      ETag: etag,
      'Cache-Control': headers['Cache-Control'],
    });
    response.end();
    return;
  }
  if (method === 'HEAD') {
    response.writeHead(200, headers);
    response.end();
    return;
  }
  response.writeHead(200, headers);
  createReadStream(found.path).pipe(response);
}

/**
 * Runs the server. Everything below lives in a function rather than at module top level so the
 * tests can import the policy builders (`buildContentSecurityPolicy`, `inlineScriptDigests`)
 * without binding a port; when this file is the program — which is what the container image and
 * `pnpm --filter @migo/web start` run — `main` is the only thing that happens.
 */
async function main() {
  const options = readOptions(process.argv.slice(2));

  try {
    const info = await stat(options.dir);
    if (!info.isDirectory()) {
      throw new Error('not a directory');
    }
  } catch {
    process.stderr.write(
      `migo-web: ${options.dir} is missing. Run \`pnpm --filter @migo/web build\` first.\n`,
    );
    process.exit(1);
  }

  // The policy is computed from the exact bytes this process is about to serve, so the digests and
  // the HTML can never disagree. A failure here is a startup failure on purpose: serving a policy
  // that does not match the bundle would brick the app in a way a crashed container announces.
  contentSecurityPolicy = buildContentSecurityPolicy(await inlineScriptDigests(options.dir));

  const server = createServer((request, response) => {
    handle(request, response, options.dir).catch(() => {
      // Never leak the cause: it would name filesystem paths. The client's action is the same either way.
      if (!response.headersSent) {
        fail(response, 500, request.method ?? 'GET');
      } else {
        response.destroy();
      }
    });
  });

  // A slow client must not be able to hold a connection open forever, and a request whose headers never
  // finish arriving must not occupy a socket. Both are the same class of trivially cheap denial of
  // service that a default-configured Node server accepts.
  server.keepAliveTimeout = 30_000;
  server.headersTimeout = 35_000;
  server.requestTimeout = 60_000;

  server.listen(options.port, options.host, () => {
    process.stdout.write(
      `migo-web serving ${options.dir} on http://${options.host}:${options.port}\n`,
    );
  });

  // SIGTERM is how a container is asked to stop. Closing the server rather than being killed lets
  // in-flight responses finish.
  for (const signal of ['SIGTERM', 'SIGINT']) {
    process.on(signal, () => {
      server.close(() => process.exit(0));
    });
  }
}

const invokedAsProgram =
  process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (invokedAsProgram) {
  await main();
}
