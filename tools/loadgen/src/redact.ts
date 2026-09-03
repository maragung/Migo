/**
 * Redaction for anything the tool prints or logs.
 *
 * A load-test report and its progress log are routinely pasted into tickets, CI output and chat, so
 * neither may carry a credential or a full IP address — even when the operator (harmlessly) put
 * basic-auth in `--api-url`, or pointed the tool at a bare IP. Two entry points cover the two
 * shapes of output: {@link sanitizeUrl} cleans a single URL for a structured field (the report's
 * server line), and {@link redact} scrubs a free-form diagnostic string (every logger line).
 *
 * Both err toward leaving normal text untouched: the patterns match only unmistakable secrets —
 * URL userinfo, a value under a secret-looking key, a Bearer token — so a benign message like
 * `connected 5/10` passes through byte-for-byte.
 */

/** Scheme prefix of an absolute URL, e.g. `http://` or `ws://`, captured so it can be kept. */
const SCHEME = '[a-z][a-z0-9+.-]*://';

/** `//user:pass@` or `//user@` immediately after a scheme — the credential half of a URL authority. */
const URL_USERINFO = new RegExp(`(${SCHEME})[^/@\\s]*@`, 'gi');

/** An IPv4 host sitting in the authority, split so the last octet can be masked. */
const URL_IPV4_HOST = new RegExp(`^(${SCHEME})(\\d{1,3}\\.\\d{1,3}\\.\\d{1,3})\\.\\d{1,3}`, 'i');

/** An IPv6 literal host, e.g. `//[2001:db8::1]`. */
const URL_IPV6_HOST = new RegExp(`^(${SCHEME})\\[[0-9a-f:]+\\]`, 'i');

/** Field names whose value is a secret. Bare `key`/`id` are intentionally excluded to avoid noise. */
const SECRET_KEY =
  'passphrase|passwd|pwd|secret|token|api[-_]?key|apikey|access[-_]?key|authorization|auth|credential|private[-_]?key';

/** `secret_key=value` or `secret_key: value`, capturing the label so only the value is masked. */
const SECRET_ASSIGNMENT = new RegExp(`(\\b(?:${SECRET_KEY})\\b\\s*[=:]\\s*)(\\S+)`, 'gi');

/** A bearer token in an Authorization header or log line. */
const BEARER = /\bBearer\s+\S+/gi;

const REDACTED = '[redacted]';

/**
 * Clean a single URL for display: drop any userinfo (so a credential embedded in the URL never
 * shows) and mask a literal IP host (so no full IP address is printed), while leaving a normal
 * hostname URL — the common case, `http://localhost:8080` — untouched. Regex-based on purpose:
 * round-tripping through `URL` would rewrite the string (a trailing slash, case folding) and change
 * output that is otherwise fine.
 */
export function sanitizeUrl(raw: string): string {
  let out = raw.replace(URL_USERINFO, '$1');
  out = out.replace(URL_IPV6_HOST, '$1[redacted-ipv6]');
  out = out.replace(URL_IPV4_HOST, '$1$2.x');
  return out;
}

/**
 * Scrub a free-form diagnostic string: strip URL userinfo, mask any value under a secret-looking
 * key, and mask bearer tokens. Bearer first, so an `Authorization: Bearer …` line loses the token
 * even though `authorization` is also a secret key.
 */
export function redact(message: string): string {
  let out = message.replace(URL_USERINFO, '$1');
  out = out.replace(BEARER, `Bearer ${REDACTED}`);
  out = out.replace(SECRET_ASSIGNMENT, `$1${REDACTED}`);
  return out;
}
