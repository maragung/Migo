/**
 * The inner plaintext: what a message *is*, before it is sealed.
 *
 * Section 11 fixes the shape of the bytes inside the ciphertext — a `content_type` byte, an
 * MSE-encoded body whose struct is chosen by that byte, and optional fixed-bucket padding — and
 * forbids JSON there. This module is that layer. The server never sees any of it; only the two ends
 * do, so unlike the protocol structs in `@migo/protocol` this vocabulary is a client-to-client
 * contract, versioned by the `content_type` values below rather than by the wire schema.
 *
 * # Why padding
 *
 * Ciphertext length leaks through the envelope even though its content does not: "sealed 12 bytes"
 * and "sealed 4000 bytes" are different observations to anyone counting bytes on the wire. Rounding
 * the plaintext up to one of a few fixed buckets before sealing collapses many lengths into one, so
 * a short "yes" and a short "no" — and every message up to the bucket size — are indistinguishable by
 * length. It costs bytes, which section 11 acknowledges by making padding OPTIONAL; the SDK pads by
 * default and lets a bandwidth-critical caller turn it off.
 *
 * # Why the body needs no length prefix
 *
 * The layout is `content_type || body || padding` with nothing marking where the body ends. It does
 * not need to: every MSE field is self-delimiting (fixed width, or length-prefixed, or an explicit
 * optional count), so decoding the body consumes exactly its own bytes and leaves the padding
 * untouched. The decoder reads the struct and simply ignores whatever follows.
 */

import { Reader, Writer, idToBytes } from '@migo/wire';
import type { Id } from '@migo/wire';

import { SdkError } from './errors.js';

/**
 * The kind of a decrypted message body.
 *
 * A distinct byte space from the protocol's `MessageKind`: `MessageKind` travels in cleartext on
 * `MessageSend`/`MessageEvent` so the server can route and count by coarse kind, while this byte
 * lives inside the ciphertext and names the exact struct that follows. They are related — a
 * {@link ContentType.Text} body rides in a `MessageKind.Text` message — but they version separately.
 */
export enum ContentType {
  /** A written message, with optional inline mentions. */
  Text = 1,
  /** A reference to an encrypted media object in storage, with the key to open it. */
  MediaRef = 2,
  /** A reference to an encrypted voice note, with its waveform and duration. */
  VoiceNoteRef = 3,
  /** An emoji reaction to another message, or the removal of one. */
  Reaction = 4,
  /** An out-of-band control signal (edits, key-exchange payloads, ephemeral markers). */
  ControlEvent = 5,
}

/** A written message. `mentions` names the users referenced inline, for client-side highlighting. */
export interface TextContent {
  type: ContentType.Text;
  text: string;
  mentions?: Id[];
}

/**
 * A pointer to an encrypted blob in object storage.
 *
 * The server stores and serves the ciphertext by `mediaId` but cannot read it: the symmetric `key`
 * and `nonce` that open it travel only here, inside the message's own ciphertext. `mimeType` and the
 * dimensions are the sender's claim and must be re-validated after decryption (section 122).
 */
export interface MediaRefContent {
  type: ContentType.MediaRef;
  mediaId: Id;
  mimeType: string;
  sizeBytes: number;
  key: Uint8Array;
  nonce: Uint8Array;
  width?: number;
  height?: number;
  blurhash?: string;
  caption?: string;
}

/** A pointer to an encrypted voice note. `waveform` is a coarse amplitude preview for the UI. */
export interface VoiceNoteRefContent {
  type: ContentType.VoiceNoteRef;
  mediaId: Id;
  mimeType: string;
  sizeBytes: number;
  durationMs: number;
  key: Uint8Array;
  nonce: Uint8Array;
  waveform?: Uint8Array;
}

/** An emoji reaction to a message. `remove` true retracts a reaction the sender placed earlier. */
export interface ReactionContent {
  type: ContentType.Reaction;
  targetMessageId: Id;
  emoji: string;
  remove: boolean;
}

/**
 * An out-of-band signal that is not itself a chat message.
 *
 * `event` names the signal (`"edit"`, `"sender-key"`, `"revoke"`); `data` is an opaque body the
 * handler for that event interprets. The sender-key distribution the group layer sends rides here.
 */
export interface ControlEventContent {
  type: ContentType.ControlEvent;
  event: string;
  data?: Uint8Array;
}

/** Any decrypted message body. Discriminated by {@link ContentType} on the `type` field. */
export type MessageContent =
  TextContent | MediaRefContent | VoiceNoteRefContent | ReactionContent | ControlEventContent;

/** How to pad the plaintext before sealing. */
export interface ContentEncodeOptions {
  /** Round the plaintext up to a fixed length bucket to blunt length analysis. Defaults to true. */
  pad?: boolean;
}

/**
 * The length buckets plaintext is rounded up to when padding is on.
 *
 * Small enough at the low end that a one-word reply and a sentence look identical, and coarse at the
 * high end so a large body is not fingerprinted to the byte. Anything past the largest fixed bucket
 * rounds up to the next multiple of it.
 */
const BUCKETS = [64, 256, 1024, 4096, 16384] as const;

/** The padded length for an unpadded plaintext of `length` bytes. */
function bucketFor(length: number): number {
  for (const bucket of BUCKETS) {
    if (length <= bucket) {
      return bucket;
    }
  }
  const largest = BUCKETS[BUCKETS.length - 1] as number;
  return Math.ceil(length / largest) * largest;
}

/**
 * Encodes a message body to the section 11 inner plaintext: the type byte, the MSE body, and, unless
 * disabled, zero padding up to the next bucket.
 *
 * The padding bytes are zero. They are never read back — the decoder stops at the end of the MSE
 * struct — so their value is immaterial and zero keeps the sealed ciphertext free of extra entropy
 * that might otherwise hint at the padding boundary.
 */
export function encodeContent(
  content: MessageContent,
  options: ContentEncodeOptions = {},
): Uint8Array {
  const writer = new Writer();
  encodeContentBody(writer, content);
  const body = writer.finish();

  const unpadded = 1 + body.length;
  const total = options.pad === false ? unpadded : bucketFor(unpadded);
  const out = new Uint8Array(total);
  out[0] = content.type;
  out.set(body, 1);
  return out;
}

/**
 * Decodes the section 11 inner plaintext back into a message body.
 *
 * Reads the type byte, decodes the struct for that type, and ignores any trailing padding. An
 * unknown type byte is a message from a newer client version; it surfaces as an error the caller can
 * render as "unsupported message" rather than crashing the conversation.
 */
export function decodeContent(plaintext: Uint8Array): MessageContent {
  if (plaintext.length === 0) {
    throw new SdkError('content: empty plaintext');
  }
  // The guard above proves byte 0 is present; assert it as the tag enum so the body decoder
  // switches enum-to-enum. An unknown tag survives the cast and is caught by the switch default.
  const type = plaintext[0] as ContentType;
  // The reader spans the body and any padding; the struct decode consumes only the body, and we
  // never assert full consumption, so the padding is harmlessly left unread.
  const reader = new Reader(plaintext.subarray(1));
  return decodeContentBody(type, reader);
}

/** Writes the MSE body for a content struct. */
function encodeContentBody(w: Writer, content: MessageContent): void {
  switch (content.type) {
    case ContentType.Text: {
      w.enter();
      w.str(content.text);
      let present = 0;
      if (content.mentions !== undefined) present++;
      w.u32(present);
      if (content.mentions !== undefined) {
        const mentions = content.mentions;
        w.optional(1, (sub) => {
          sub.listLen(mentions.length);
          for (const id of mentions) {
            sub.id(id);
          }
        });
      }
      w.leave();
      return;
    }
    case ContentType.MediaRef: {
      w.enter();
      w.id(content.mediaId);
      w.str(content.mimeType);
      w.u64(content.sizeBytes);
      w.bytes(content.key);
      w.bytes(content.nonce);
      let present = 0;
      if (content.width !== undefined) present++;
      if (content.height !== undefined) present++;
      if (content.blurhash !== undefined) present++;
      if (content.caption !== undefined) present++;
      w.u32(present);
      if (content.width !== undefined) {
        const width = content.width;
        w.optional(1, (sub) => sub.u32(width));
      }
      if (content.height !== undefined) {
        const height = content.height;
        w.optional(2, (sub) => sub.u32(height));
      }
      if (content.blurhash !== undefined) {
        const blurhash = content.blurhash;
        w.optional(3, (sub) => sub.str(blurhash));
      }
      if (content.caption !== undefined) {
        const caption = content.caption;
        w.optional(4, (sub) => sub.str(caption));
      }
      w.leave();
      return;
    }
    case ContentType.VoiceNoteRef: {
      w.enter();
      w.id(content.mediaId);
      w.str(content.mimeType);
      w.u64(content.sizeBytes);
      w.u32(content.durationMs);
      w.bytes(content.key);
      w.bytes(content.nonce);
      let present = 0;
      if (content.waveform !== undefined) present++;
      w.u32(present);
      if (content.waveform !== undefined) {
        const waveform = content.waveform;
        w.optional(1, (sub) => sub.bytes(waveform));
      }
      w.leave();
      return;
    }
    case ContentType.Reaction: {
      w.enter();
      w.id(content.targetMessageId);
      w.str(content.emoji);
      w.bool(content.remove);
      w.u32(0);
      w.leave();
      return;
    }
    case ContentType.ControlEvent: {
      w.enter();
      w.str(content.event);
      let present = 0;
      if (content.data !== undefined) present++;
      w.u32(present);
      if (content.data !== undefined) {
        const data = content.data;
        w.optional(1, (sub) => sub.bytes(data));
      }
      w.leave();
      return;
    }
    default: {
      // Exhaustiveness: a new ContentType with no encode arm is a compile error here.
      const unreachable: never = content;
      throw new SdkError(`content: unencodable body ${JSON.stringify(unreachable)}`);
    }
  }
}

/** Reads the MSE body for a content struct of the given type. */
function decodeContentBody(type: ContentType, r: Reader): MessageContent {
  switch (type) {
    case ContentType.Text: {
      r.enter();
      const text = r.str();
      const content: TextContent = { type: ContentType.Text, text };
      const optionalCount = r.u32();
      for (let i = 0; i < optionalCount; i++) {
        const [fieldId, sub] = r.optional();
        if (fieldId === 1) {
          const count = sub.listLen();
          const mentions: Id[] = [];
          for (let m = 0; m < count; m++) {
            mentions.push(sub.id());
          }
          content.mentions = mentions;
        }
      }
      r.leave();
      return content;
    }
    case ContentType.MediaRef: {
      r.enter();
      const mediaId = r.id();
      const mimeType = r.str();
      const sizeBytes = r.u64();
      const key = r.bytes();
      const nonce = r.bytes();
      const content: MediaRefContent = {
        type: ContentType.MediaRef,
        mediaId,
        mimeType,
        sizeBytes,
        key,
        nonce,
      };
      const optionalCount = r.u32();
      for (let i = 0; i < optionalCount; i++) {
        const [fieldId, sub] = r.optional();
        switch (fieldId) {
          case 1:
            content.width = sub.u32();
            break;
          case 2:
            content.height = sub.u32();
            break;
          case 3:
            content.blurhash = sub.str();
            break;
          case 4:
            content.caption = sub.str();
            break;
          default:
            break;
        }
      }
      r.leave();
      return content;
    }
    case ContentType.VoiceNoteRef: {
      r.enter();
      const mediaId = r.id();
      const mimeType = r.str();
      const sizeBytes = r.u64();
      const durationMs = r.u32();
      const key = r.bytes();
      const nonce = r.bytes();
      const content: VoiceNoteRefContent = {
        type: ContentType.VoiceNoteRef,
        mediaId,
        mimeType,
        sizeBytes,
        durationMs,
        key,
        nonce,
      };
      const optionalCount = r.u32();
      for (let i = 0; i < optionalCount; i++) {
        const [fieldId, sub] = r.optional();
        if (fieldId === 1) {
          content.waveform = sub.bytes();
        }
      }
      r.leave();
      return content;
    }
    case ContentType.Reaction: {
      r.enter();
      const targetMessageId = r.id();
      const emoji = r.str();
      const remove = r.bool();
      const optionalCount = r.u32();
      for (let i = 0; i < optionalCount; i++) {
        r.optional();
      }
      r.leave();
      return { type: ContentType.Reaction, targetMessageId, emoji, remove };
    }
    case ContentType.ControlEvent: {
      r.enter();
      const event = r.str();
      const content: ControlEventContent = { type: ContentType.ControlEvent, event };
      const optionalCount = r.u32();
      for (let i = 0; i < optionalCount; i++) {
        const [fieldId, sub] = r.optional();
        if (fieldId === 1) {
          content.data = sub.bytes();
        }
      }
      r.leave();
      return content;
    }
    default: {
      // `type` narrows to `never` here because the switch is exhaustive over the enum, but the
      // tag came off the wire and may name a content type this build does not know. Widen it back
      // to the raw number (a plain assignment, no assertion) to report the offending value.
      const unknownTag: number = type;
      throw new SdkError(`content: unsupported content_type ${unknownTag}`);
    }
  }
}

/** The 16 conversation bytes used as group associated data when sealing (see the group layer). */
export function conversationContext(conversationId: Id): Uint8Array {
  return idToBytes(conversationId);
}
