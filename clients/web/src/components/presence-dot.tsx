import type { ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';

const CLASS_BY_STATE: Record<number, string> = {
  [PresenceState.Online]: 'presence-online',
  [PresenceState.Away]: 'presence-away',
  [PresenceState.Busy]: 'presence-busy',
  [PresenceState.Offline]: 'presence-offline',
};

/** A small coloured dot for a presence state; renders nothing for unknown/invisible. */
export function PresenceDot({ state }: { state: PresenceState | undefined }): ReactNode {
  if (state === undefined) {
    return null;
  }
  const cls = CLASS_BY_STATE[state];
  if (!cls) {
    return null;
  }
  return <span className={`presence-dot ${cls}`} aria-hidden="true" />;
}
