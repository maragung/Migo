'use client';

/**
 * Section navigation for surfaces that do not own the shell's state.
 *
 * The shell owns the current section, but a thread inside the content area can also ask for a
 * section: a $MIG token reference in message text opens the Wallet. The shell provides this
 * context; any descendant calls {@link useSectionNav} instead of threading callbacks through
 * the route's children (which the shell cannot do — the content area is `children`, and props
 * do not travel through it).
 */

import { createContext, useContext } from 'react';
import type { ReactNode } from 'react';

import type { AppTab } from '@/components/app-shell.js';

/** The shell's promise: switch the app to a section. */
export type SectionNav = (tab: AppTab) => void;

const SectionNavContext = createContext<SectionNav | null>(null);

/** Provides the shell's section switch to the content below it. */
export function SectionNavProvider({
  navigate,
  children,
}: {
  navigate: SectionNav;
  children: ReactNode;
}): ReactNode {
  return <SectionNavContext.Provider value={navigate}>{children}</SectionNavContext.Provider>;
}

/** The shell's section switch, for any surface that needs to open a section. */
export function useSectionNav(): SectionNav {
  const value = useContext(SectionNavContext);
  if (value === null) {
    throw new Error('useSectionNav must be used within a SectionNavProvider');
  }
  return value;
}
