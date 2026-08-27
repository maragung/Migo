import assert from 'node:assert/strict';
import test from 'node:test';

import { formatRelative } from '../src/lib/format.js';
import { webHello } from '../src/lib/migo/hello.js';

test('smoke: a pure helper is importable and runs', () => {
  assert.equal(formatRelative(1_000, 1_000), 'now');
});

test('smoke: an @/-aliased source module resolves through the loader', () => {
  // hello.ts imports `@/lib/config.js`; if this returns a hello object the alias resolved.
  assert.equal(webHello().platform, 1);
});
