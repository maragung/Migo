'use client';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { useProfile } from '@/lib/migo/use-profiles.js';

/** Shows an animated "typing" line for the given user, or reserves the space when nobody is typing. */
export function TypingIndicator({ userId }: { userId: Id | null }): ReactNode {
  const profile = useProfile(userId);
  if (!userId) {
    return <div className="typing-indicator" aria-hidden="true" />;
  }
  const name = profile?.displayName ?? 'Someone';
  return (
    <div className="typing-indicator" aria-live="polite">
      <span>{name} is typing</span>
      <span className="typing-dots">
        <span />
        <span />
        <span />
      </span>
    </div>
  );
}
