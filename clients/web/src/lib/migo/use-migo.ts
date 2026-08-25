'use client';

/** Access to the Migo client context. Throws if used outside {@link MigoProvider}. */

import { useContext } from 'react';

import { MigoContext } from './provider.js';
import type { MigoContextValue } from './provider.js';

export function useMigo(): MigoContextValue {
  const value = useContext(MigoContext);
  if (value === null) {
    throw new Error('useMigo must be used within a MigoProvider');
  }
  return value;
}
