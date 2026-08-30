'use client';

/**
 * Re-seals message content for the edit and reaction paths.
 *
 * {@link MessagingDomain.send} seals content with its own private group-crypto instance, but
 * {@link MessagingDomain.editMessage} and {@link MessagingDomain.sendReaction} take the sealed
 * envelope as a caller-supplied argument — the bytes pass through verbatim, so the caller must
 * produce them. This module is that caller: one {@link GroupCrypto} bound to the live client's
 * key store (the same identity the sending path signs with), memoized per key store so repeat
 * edits reuse one outbound chain instead of minting a fresh chain per call.
 *
 * # The seam, stated honestly
 *
 * The memoized instance here is not the messaging domain's own instance, so a replacement sealed
 * here rides a chain the domain has not distributed. The task this serves calls for a simple
 * re-encoding now; when the SDK grows a public seal-for-edit surface, this module is the single
 * place to switch to it.
 */

import { ContentType, GroupCrypto, encodeContent } from '@migo/sdk';
import type { Id, MigoClient, MessageContent, ReactionContent, TextContent } from '@migo/sdk';

/** One sealer per key store, so a session's replacement envelopes share one outbound chain. */
const sealers = new WeakMap<object, GroupCrypto>();

function sealerFor(client: MigoClient): GroupCrypto {
  const keys = client.keyStore;
  let sealer = sealers.get(keys);
  if (sealer === undefined) {
    sealer = new GroupCrypto(keys);
    sealers.set(keys, sealer);
  }
  return sealer;
}

/** Seals `content` exactly as a send would: encoded plaintext under the conversation's chain. */
function seal(client: MigoClient, conversationId: Id, content: MessageContent): Uint8Array {
  const plaintext = encodeContent(content);
  return sealerFor(client).sealContent(conversationId, plaintext).envelope;
}

/** The sealed replacement envelope for an edited text message. */
export function sealTextEdit(client: MigoClient, conversationId: Id, text: string): Uint8Array {
  const content: TextContent = { type: ContentType.Text, text };
  return seal(client, conversationId, content);
}

/** The sealed reaction envelope for a message: the emoji never rides the wire in the clear. */
export function sealReaction(
  client: MigoClient,
  conversationId: Id,
  targetMessageId: Id,
  emoji: string,
): Uint8Array {
  const content: ReactionContent = {
    type: ContentType.Reaction,
    targetMessageId,
    emoji,
    remove: false,
  };
  return seal(client, conversationId, content);
}
