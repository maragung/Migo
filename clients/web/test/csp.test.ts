/**
 * The Content-Security-Policy, as one policy in two homes.
 *
 * §108 makes the CSP part of the E2E security model, which means it cannot quietly rot. It is
 * written twice on purpose: `tools/serve.mjs` sends it as a response header (with the bundle's
 * inline-script hashes, which only the server serving those bytes can compute), and the root
 * layout carries a meta policy so the bundle keeps its guard under any host that sends no header.
 * Two homes means two ways to drift, so this file reads both and holds them to the contract the
 * layout's comment states:
 *
 *   * every meta directive appears in the header policy with the same value — the meta is the
 *     header policy's stable subset;
 *   * the meta omits exactly the three directives it must: `script-src` (and `default-src`, which
 *     would become it) because Next's App Router embeds build-dependent inline hydration scripts
 *     a static file cannot hash-pin, and `frame-ancestors`, which the spec ignores in a meta; and
 *   * neither policy ever says `unsafe-eval`, and the script policy never says `unsafe-inline` —
 *     the two concessions the brief forbids outright.
 *
 * The digest scan itself is pinned against a hand-built bundle directory: inline scripts hashed
 * byte for byte, external scripts and nested pages handled, and importing serve.mjs must not
 * start a server.
 */

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

const serveUrl = new URL('../../tools/serve.mjs', import.meta.url);
const layoutUrl = new URL('../../src/app/layout.tsx', import.meta.url);

/** The policy builders, imported the way the layout's comment promises a test can reach them. */
type Serve = {
  buildContentSecurityPolicy: (digests?: string[]) => string;
  inlineScriptDigests: (root: string) => Promise<string[]>;
};

/** Parses a policy string into directive → source tokens, the shape every assertion below needs. */
function parsePolicy(policy: string): Map<string, string[]> {
  const parsed = new Map<string, string[]>();
  for (const directive of policy.split(';')) {
    const [name, ...sources] = directive.trim().split(/\s+/);
    assert.ok(name !== undefined, `a policy directive has no name: "${directive}"`);
    parsed.set(name, sources);
  }
  return parsed;
}

/** The layout's meta policy, read from the source the bundle actually ships. */
async function metaPolicy(): Promise<Map<string, string[]>> {
  const { readFile } = await import('node:fs/promises');
  const source = await readFile(layoutUrl, 'utf8');
  const declaration = source.match(/const META_CSP = \[([\s\S]*?)\]\.join/);
  assert.ok(declaration, 'src/app/layout.tsx must declare its meta policy as META_CSP');
  const items = [...(declaration[1] ?? '').matchAll(/"([^"]+)"/g)].map((match) => match[1]);
  assert.ok(items.length > 0, 'META_CSP must list at least one directive');
  return parsePolicy(items.join('; '));
}

test('importing serve.mjs exposes the policy builders without starting a server', async () => {
  const serve = (await import(serveUrl.href)) as Serve;
  assert.equal(typeof serve.buildContentSecurityPolicy, 'function');
  assert.equal(typeof serve.inlineScriptDigests, 'function');
  // The process would still be listening if the import had run main(); the test runner's clean
  // exit is the assertion.
});

test("the layout meta policy is the header policy's stable subset", async () => {
  const serve = (await import(serveUrl.href)) as Serve;
  const header = parsePolicy(serve.buildContentSecurityPolicy());
  const meta = await metaPolicy();

  for (const [name, sources] of meta) {
    assert.ok(
      header.has(name),
      `meta directive ${name} is missing from the serve.mjs policy — the two have drifted`,
    );
    assert.deepEqual(
      header.get(name),
      sources,
      `directive ${name} must carry the same sources in both policies`,
    );
  }
});

test('the meta omits exactly the directives it cannot carry, and the header supplies them', async () => {
  const serve = (await import(serveUrl.href)) as Serve;
  const header = parsePolicy(serve.buildContentSecurityPolicy());
  const meta = await metaPolicy();

  // A meta `script-src 'self'` (or a `default-src` that becomes one) does not harden the page —
  // it bricks hydration, because Next's inline flight scripts are unnamed until the server hashes
  // them. These omissions are the design, not an oversight, which is why they are pinned.
  assert.equal(meta.has('script-src'), false, 'the meta must not carry script-src');
  assert.equal(meta.has('default-src'), false, 'the meta must not carry default-src');
  assert.equal(meta.has('frame-ancestors'), false, 'a meta policy cannot carry frame-ancestors');

  // And the header is where those three live.
  assert.deepEqual(header.get('default-src'), ["'self'"]);
  assert.deepEqual(header.get('frame-ancestors'), ["'none'"]);
});

test('no policy ships the two concessions the brief forbids', async () => {
  const serve = (await import(serveUrl.href)) as Serve;
  const header = parsePolicy(serve.buildContentSecurityPolicy());
  const meta = await metaPolicy();

  const scriptTokens = header.get('script-src') ?? [];
  assert.equal(
    scriptTokens.includes("'unsafe-inline'"),
    false,
    'script-src must never allow unsafe-inline',
  );
  for (const [name, sources] of [...header, ...meta]) {
    assert.equal(sources.includes("'unsafe-eval'"), false, `${name} must never allow unsafe-eval`);
  }
});

test('script-src is self plus one sha256 per inline script, and nothing else', async () => {
  const serve = (await import(serveUrl.href)) as Serve;

  // No digests (a hypothetical bundle with no inline scripts): self, alone.
  assert.equal(serve.buildContentSecurityPolicy(), serve.buildContentSecurityPolicy([]));
  assert.ok(serve.buildContentSecurityPolicy().includes("script-src 'self';"));

  // With digests: self first, then base64 SHA-256 tokens, deduplicated.
  const withHashes = serve.buildContentSecurityPolicy(['digest-a', 'digest-a', 'digest-b']);
  const tokens = parsePolicy(withHashes).get('script-src') ?? [];
  assert.deepEqual(tokens, ["'self'", 'sha256-digest-a', 'sha256-digest-b']);
});

test('the digest scan hashes inline scripts byte for byte and ignores external ones', async () => {
  const serve = (await import(serveUrl.href)) as Serve;
  const dir = await mkdtemp(join(tmpdir(), 'migo-csp-'));
  try {
    await mkdir(join(dir, 'chat'));
    await writeFile(
      join(dir, 'index.html'),
      [
        '<!doctype html><html><head><script src="/theme-init.js"></script></head><body>',
        '<script>self.__next_f.push([1,"first"]);</script>',
        '<script>self.__next_f.push([1,"second"]);</script>',
        // A script with a src attribute and a body is external; its body is not hashed.
        '<script src="/x.js">never hashed</script>',
        '</body></html>',
      ].join('\n'),
    );
    await writeFile(
      join(dir, 'chat', 'index.html'),
      '<html><body><script>self.__next_f.push([1,"nested"]);</script></body></html>',
    );
    // A non-HTML file must not be scanned at all.
    await writeFile(join(dir, 'sw.js'), '<script>not html at all</script>');

    const digestOf = (text: string): string =>
      createHash('sha256').update(text, 'utf8').digest('base64');

    // Files are scanned in sorted path order (chat/index.html before index.html), so the policy a
    // restart builds is byte-identical to the one before it.
    assert.deepEqual(await serve.inlineScriptDigests(dir), [
      digestOf('self.__next_f.push([1,"nested"]);'),
      digestOf('self.__next_f.push([1,"first"]);'),
      digestOf('self.__next_f.push([1,"second"]);'),
    ]);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('the pre-paint theme restore is a same-origin file, not an inline script', async () => {
  const { readFile } = await import('node:fs/promises');
  const layout = await readFile(layoutUrl, 'utf8');
  assert.equal(
    layout.includes('dangerouslySetInnerHTML'),
    false,
    'the layout must not inline any script — the CSP admits none',
  );
  // The external reference is what keeps the pre-paint restore inside script-src 'self'.
  assert.ok(
    layout.includes('<script src="/theme-init.js" />'),
    'the layout must load public/theme-init.js as a same-origin script',
  );
  const init = await readFile(new URL('../../public/theme-init.js', import.meta.url), 'utf8');
  assert.ok(
    init.includes("localStorage.getItem('migo:theme')"),
    'the external init must read the same storage key as the theme module',
  );
});
