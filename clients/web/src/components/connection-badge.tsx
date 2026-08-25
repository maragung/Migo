'use client';

import type { ReactNode } from 'react';

import type { ConnectionState } from '@migo/sdk';

import { useMigo } from '@/lib/migo/use-migo.js';

function describe(state: ConnectionState): { cls: string; label: string } | null {
  switch (state) {
    case 'ready':
      // A healthy connection needs no badge.
      return null;
    case 'connecting':
    case 'authenticating':
      return { cls: 'conn-wait', label: 'Connecting…' };
    case 'reconnecting':
      return { cls: 'conn-wait', label: 'Reconnecting…' };
    case 'idle':
    case 'closed':
    default:
      return { cls: 'conn-down', label: 'Offline' };
  }
}

/** A compact badge that appears only when the realtime connection is not healthy. */
export function ConnectionBadge(): ReactNode {
  const { connectionState } = useMigo();
  const info = describe(connectionState);
  if (!info) {
    return null;
  }
  return (
    <span className={`conn-badge ${info.cls}`}>
      <span className="dot" />
      {info.label}
    </span>
  );
}
