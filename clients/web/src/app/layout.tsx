import type { Metadata, Viewport } from 'next';
import type { ReactNode } from 'react';

import { MigoProvider } from '@/lib/migo/provider.js';

import { SwRegister } from './sw-register.js';

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
    <html lang="en">
      <body>
        <MigoProvider>{children}</MigoProvider>
        <SwRegister />
      </body>
    </html>
  );
}
