import type { Metadata, Viewport } from 'next';
import type { ReactNode } from 'react';

import { MigoProvider } from '@/lib/migo/provider.js';

import { SwRegister } from './sw-register.js';
import { ThemeFollower } from './theme-follower.js';

import './globals.css';

/**
 * The Content-Security-Policy the bundle carries wherever it is served (§57, §108, §164: "CSP
 * bukan pelengkap, tetapi bagian dari model keamanan E2E di web").
 *
 * `tools/serve.mjs` — the production server — sends this same policy as a response header with
 * two additions it can compute and a static file cannot: `script-src 'self'` plus a SHA-256 per
 * inline script, and `frame-ancestors 'none'` (which a meta policy cannot carry). The two must
 * stay aligned; `test/csp.test.ts` compares them so they cannot drift.
 *
 * What is deliberately *absent* here, and why:
 *
 *   * `script-src` (and `default-src`, which would become it). Next's App Router embeds its
 *     hydration payload as inline `<script>self.__next_f.push(...)</script>` chunks whose bytes
 *     differ per build, so a build-independent policy cannot name them — and a meta policy that
 *     says `script-src 'self'` with them unnamed does not harden the page, it bricks hydration
 *     under any host. Under serve.mjs the header supplies the strict script policy (with each
 *     inline script hash-pinned from the exact bytes being served); under a host that sends no
 *     header, this meta still governs every other channel below.
 *   * `frame-ancestors` — the spec ignores it in a meta policy. `X-Frame-Options: DENY` from the
 *     server covers the framing case.
 *
 * The two directives a reader will squint at:
 *
 *   * `connect-src 'self' http: https: ws: wss:` — scheme-wide rather than origin-listed, because
 *     the REST and gateway origins are *user-configured* (the server a person points their client
 *     at, chosen at sign-in and persisted in IndexedDB). A strict CSP cannot enumerate origins
 *     that do not exist until a user types them; scheme-only is the honest ceiling, and the API
 *     origin is a peer the app must be able to reach, not a script source to be fenced.
 *   * `style-src 'unsafe-inline'` — the components position context menus, presence dots and
 *     progress bars through `style={{}}` props, which the DOM turns into `style` attributes, and
 *     CSP3 gates those under style-src with no same-origin alternative. React cannot move them
 *     to classes. This is the one concession in the policy; it buys style placement, never
 *     script execution, and there is no <style> tag anywhere in the source to hide behind it.
 */
const META_CSP = [
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
].join('; ');

export const metadata: Metadata = {
  title: 'Migo',
  description: 'Private, end-to-end encrypted messaging.',
  applicationName: 'Migo',
  manifest: '/manifest.webmanifest',
  appleWebApp: {
    capable: true,
    title: 'Migo',
    statusBarStyle: 'black-translucent',
  },
  icons: {
    icon: '/icons/icon.svg',
    apple: '/icons/icon.svg',
  },
};

export const viewport: Viewport = {
  themeColor: '#141519',
  width: 'device-width',
  initialScale: 1,
  maximumScale: 1,
  viewportFit: 'cover',
  // Android's default is `resizes-visual`: the on-screen keyboard covers the layout viewport,
  // so the composer sat under it and the message list kept its full height. Resizing the
  // layout instead shrinks the shell to the visible viewport, which is what a messenger
  // needs — the input rides just above the keyboard. (iOS ignores the key.)
  interactiveWidget: 'resizes-content',
};

export default function RootLayout({ children }: { children: ReactNode }): ReactNode {
  return (
    // `data-theme="dark"` is the server-rendered default; the theme-init script below restores the
    // visitor's stored choice before first paint, so `suppressHydrationWarning` covers the one
    // attribute the script may have rewritten by the time React hydrates.
    <html lang="en" data-theme="dark" suppressHydrationWarning>
      <body>
        {/* The pre-paint theme restore (see public/theme-init.js), loaded as a same-origin file
            rather than inlined: the CSP above admits no inline script. It is a plain blocking
            script at the top of <body>, so it still runs during parse, before anything below it
            paints — the property the inline version existed for. React hoists the <meta> into
            <head>, where a meta CSP is honoured. */}
        <script src="/theme-init.js" />
        <meta httpEquiv="Content-Security-Policy" content={META_CSP} />
        <MigoProvider>{children}</MigoProvider>
        <SwRegister />
        <ThemeFollower />
      </body>
    </html>
  );
}
