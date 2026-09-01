/**
 * Stamps the built service worker's cache name with this build's source stamp.
 *
 * `public/sw.js` ships a fixed placeholder cache name; every deploy must change the worker's
 * bytes so browsers pick the update up, and the worker's activate step then deletes every cache
 * whose name is not its own. Without the stamp, a deploy that only changed hashed chunks left
 * old caches in place forever, and a browser that had cached the shell could keep describing
 * itself as current — the user sees the previous release's UI with no error anywhere.
 *
 * The stamp is the git SHA CI builds from; a tree without git (or a local sandbox) falls back to
 * a timestamp, which still guarantees the name never repeats.
 */

import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';

const swUrl = new URL('../out/sw.js', import.meta.url);
const PLACEHOLDER = "const CACHE = 'migo-web-v1';";

let stamp = '';
try {
  stamp = execFileSync('git', ['rev-parse', '--short', 'HEAD'], { encoding: 'utf8' }).trim();
} catch {
  stamp = `t${new Date().getTime().toString(36)}`;
}
if (stamp === '') {
  stamp = `t${new Date().getTime().toString(36)}`;
}

const source = readFileSync(swUrl, 'utf8');
if (!source.includes(PLACEHOLDER)) {
  console.error(
    'stamp-sw: out/sw.js no longer carries the placeholder cache name; update the script',
  );
  process.exit(1);
}
writeFileSync(swUrl, source.replace(PLACEHOLDER, `const CACHE = 'migo-web-${stamp}';`));
console.log(`stamp-sw: cache stamped migo-web-${stamp}`);
