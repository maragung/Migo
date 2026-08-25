'use client';

import type { ReactNode } from 'react';
import { useParams } from 'next/navigation';

import type { Id } from '@migo/sdk';

import { ChatWindow } from '@/components/chat-window.js';

/** Renders the open conversation identified by the route's [id] segment. */
export default function ConversationPage(): ReactNode {
  const params = useParams<{ id: string }>();
  const raw = params.id;
  const id = (Array.isArray(raw) ? raw[0] : raw) as Id | undefined;
  if (!id) {
    return null;
  }
  return <ChatWindow conversationId={id} />;
}
