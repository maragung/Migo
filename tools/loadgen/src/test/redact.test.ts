/**
 * The redaction that keeps a credential or a full IP out of any report or log line.
 *
 * These are security assertions, so they are pinned two ways at once: the secret substring must be
 * absent from the output, and a redaction marker must be present — a rule that only deletes the
 * marker, or only leaves the secret, fails one side. The benign cases matter just as much: a
 * redactor that mangles `connected 5/10` would train operators to distrust the log and paste the
 * raw values back in.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { redact, sanitizeUrl } from '../redact.js';

test('sanitizeUrl leaves an ordinary hostname URL byte-for-byte unchanged', () => {
  assert.equal(sanitizeUrl('http://localhost:8080'), 'http://localhost:8080');
  assert.equal(sanitizeUrl('ws://localhost:8080/ws'), 'ws://localhost:8080/ws');
  assert.equal(sanitizeUrl('https://gateway.example.com/ws'), 'https://gateway.example.com/ws');
});

test('sanitizeUrl drops userinfo so a URL-embedded credential never shows', () => {
  const out = sanitizeUrl('http://admin:s3cr3t@api.example.com:8080');
  assert.equal(out, 'http://api.example.com:8080');
  assert.ok(!out.includes('s3cr3t'));
  assert.ok(!out.includes('admin'));
});

test('sanitizeUrl masks the last octet of an IPv4 host so no full IP is printed', () => {
  const out = sanitizeUrl('http://198.51.100.7:8080');
  assert.equal(out, 'http://198.51.100.x:8080');
  assert.ok(!out.includes('198.51.100.7'));
});

test('sanitizeUrl handles userinfo and an IPv4 host together', () => {
  const out = sanitizeUrl('http://admin:s3cr3t@198.51.100.7:8080');
  assert.equal(out, 'http://198.51.100.x:8080');
  assert.ok(!out.includes('s3cr3t'));
  assert.ok(!out.includes('198.51.100.7'));
});

test('sanitizeUrl redacts an IPv6 literal host', () => {
  const out = sanitizeUrl('http://[2001:db8::1]:8080/ws');
  assert.ok(!out.includes('2001:db8::1'));
  assert.ok(out.includes('[redacted-ipv6]'));
});

test('redact strips userinfo from a URL embedded in a sentence', () => {
  const out = redact('could not read http://svc:p4ssw0rd@host/v1/config: connection refused');
  assert.ok(!out.includes('p4ssw0rd'));
  assert.ok(!out.includes('svc:p4ssw0rd'));
  assert.ok(out.includes('connection refused'));
});

test('redact masks the value under a secret-looking key', () => {
  for (const [input, secret] of [
    ['password=hunter2', 'hunter2'],
    ['token: deadbeefcafebabe0123', 'deadbeefcafebabe0123'],
    ['api_key=AKIA0123456789ABCDEF', 'AKIA0123456789ABCDEF'],
    ['private-key: MIIEvQIBADAN', 'MIIEvQIBADAN'],
  ] as const) {
    const out = redact(input);
    assert.ok(!out.includes(secret), `${secret} must be gone from ${out}`);
    assert.ok(out.includes('[redacted]'));
  }
});

test('redact masks a bearer token even under an Authorization label', () => {
  const out = redact('Authorization: Bearer eyJhbGciOi.JIUzI1NiIs.InR5cCI6');
  assert.ok(!out.includes('eyJhbGciOi.JIUzI1NiIs.InR5cCI6'));
  assert.ok(out.includes('[redacted]'));
});

test('redact leaves benign diagnostics untouched, including near-miss words', () => {
  for (const benign of [
    'connected 5/10',
    'building 10 virtual users for scenario "messaging"',
    'the author changed the key mapping', // "author" must not trip the "auth" rule
    'running for 30s across 5 active workload(s)...',
  ]) {
    assert.equal(redact(benign), benign);
  }
});
