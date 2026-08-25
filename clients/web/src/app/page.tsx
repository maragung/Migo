'use client';

import { useEffect } from 'react';
import type { ReactNode } from 'react';
import { useRouter } from 'next/navigation';

import { FullSpinner } from '@/components/spinner.js';
import { useMigo } from '@/lib/migo/use-migo.js';

/** Entry point: sends signed-in visitors to the chat shell and everyone else to the login page. */
export default function HomePage(): ReactNode {
  const { status } = useMigo();
  const router = useRouter();

  useEffect(() => {
    if (status === 'ready') {
      router.replace('/chat');
    } else if (status === 'anonymous') {
      router.replace('/login');
    }
  }, [status, router]);

  return <FullSpinner label="Loading Migo…" />;
}
