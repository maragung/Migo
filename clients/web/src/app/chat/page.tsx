'use client';

import type { ReactNode } from 'react';

/**
 * The chat route's page: nothing of its own.
 *
 * The shell is a window manager now (see components/app-shell.tsx) — a conversation opens as its
 * own window, drawn by the shell from the URL fragment, not as this route's page. The route still
 * exists so `/chat` is where a signed-in session lands; its layout carries the provider stack and
 * the shell, and this page contributes only the route's own (empty) content.
 */
export default function ChatPage(): ReactNode {
  return null;
}
