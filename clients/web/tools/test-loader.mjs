// Runtime resolver for the `@/*` path alias the browser bundle uses. Passed to `node --import`.
//
// The web client's source imports its own modules through the `@/` alias (mapped to `src/` by
// tsconfig `paths`). TypeScript does not rewrite those specifiers on emit, so the compiled test
// output still contains `import '@/lib/…'`, which Node cannot resolve on its own. This hook maps
// any `@/x` specifier to the corresponding compiled file under `dist/src/`, so a test can import a
// source module that reaches for a sibling through the alias without the graph being rebuilt around
// a Node-native specifier. Kept as a loader rather than a source edit because the alias is the real
// production layout: the tests must run the code as shipped, not a rewritten copy of it.
import { registerHooks } from 'node:module';

const distSrc = new URL('../dist/src/', import.meta.url);

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('@/')) {
      return { url: new URL(specifier.slice(2), distSrc).href, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  },
});
