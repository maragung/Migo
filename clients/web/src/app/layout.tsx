import type { Metadata, Viewport } from 'next';
import type { ReactNode } from 'react';

import { MigoProvider } from '@/lib/migo/provider.js';
import { themeInitScript } from '@/lib/theme.js';

import { SwRegister } from './sw-register.js';
import { ThemeFollower } from './theme-follower.js';

import './globals.css';

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
  themeColor: '#0b0f14',
  width: 'device-width',
  initialScale: 1,
  maximumScale: 1,
  viewportFit: 'cover',
};

export default function RootLayout({ children }: { children: ReactNode }): ReactNode {
  return (
    // `data-theme="dark"` is the server-rendered default; the inline script below restores the
    // visitor's stored choice before first paint, so `suppressHydrationWarning` covers the one
    // attribute the script may have rewritten by the time React hydrates.
    <html lang="en" data-theme="dark" suppressHydrationWarning>
      <body>
        <script dangerouslySetInnerHTML={{ __html: themeInitScript }} />
        <MigoProvider>{children}</MigoProvider>
        <SwRegister />
        <ThemeFollower />
      </body>
    </html>
  );
}
