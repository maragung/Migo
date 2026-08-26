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
// Usage: node tools/serve.mjs [--port 19991] [--host 0.0.0.0] [--dir out]
//        MIGO_WEB_PORT / PORT, MIGO_WEB_HOST / HOST, MIGO_WEB_DIR override the defaults.

import { createReadStream } from 'node:fs';
import { stat } from 'node:fs/promises';
import { createServer } from 'node:http';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

/** The port Migo's web client is served on. */
const DEFAULT_PORT = 19991;

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
 * Security headers applied to every response.
 *
 * `Cross-Origin-Opener-Policy` and `Cross-Origin-Embedder-Policy` isolate the browsing context, so a
 * window this page opens (or that opens it) cannot reach into it. The rest are the ordinary hardening
 * a static host should send and cost nothing to get right here.
 *
 * There is deliberately no `Content-Security-Policy` here: the correct policy depends on the API and
 * gateway origins this deployment talks to, which this script does not know. It belongs in the
 * deployment's reverse proxy, alongside HSTS and TLS. Stating that in the file is better than shipping
 * a policy that is wrong for every deployment but one.
 */
const SECURITY_HEADERS = {
  'X-Content-Type-Options': 'nosniff',
  'X-Frame-Options': 'DENY',
  'Referrer-Policy': 'no-referrer',
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
  'Permissions-Policy': 'geolocation=(), payment=(), usb=()',
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
