'use client';

import { useEffect } from 'react';
import type { ReactNode } from 'react';
import { useRouter } from 'next/navigation';

import { useMigo } from '@/lib/migo/use-migo.js';

import { FullSpinner } from './spinner.js';

/**
 * Gate for authenticated pages.
 *
 * While the session is still being restored it shows a spinner; once we know the visitor is signed out it
 * redirects to the login page; only a `ready` session renders the protected children.
 */
export function RequireReady({ children }: { children: ReactNode }): ReactNode {
  const { status } = useMigo();
  const router = useRouter();

  useEffect(() => {
    if (status === 'anonymous') {
      router.replace('/login');
    }
  }, [status, router]);

  if (status === 'ready') {
    return children;
  }
  return (
    <FullSpinner label={status === 'anonymous' ? 'Redirecting…' : 'Restoring your session…'} />
  );
}
