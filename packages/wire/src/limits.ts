/**
 * Protocol limits, from `shared/protocol/schema`.
 *
 * These are duplicated in `migo-wire` (Rust) and generated into `@migo/protocol` as
 * `LIMITS`; `make protocol-check` is what keeps the generated copy honest. This file
 * exists because the codec cannot import the protocol package — the protocol package
 * imports the codec — and a codec that took its limits from its caller would let a
 * peer raise them.
 *
 * The server also sends its own limits in `Welcome`, and a client must respect those
 * rather than these when the two disagree: these are the ceiling this build can parse,
 * not permission to send that much.
 */

/** Largest frame this build will encode or decode, header included. */
export const MAX_FRAME_BYTES = 262144;
/** Longest UTF-8 string field. */
export const MAX_STRING_BYTES = 65536;
/** Longest opaque byte field. */
export const MAX_BYTES_LEN = 131072;
/** Most items in a `list<T>`. */
export const MAX_LIST_ITEMS = 4096;
/** Most entries in a `map<string, T>`. */
export const MAX_MAP_ITEMS = 1024;
/** Deepest struct nesting. Bounds recursion during decode. */
export const MAX_NESTING_DEPTH = 16;
/** Most sub-frames in a BATCH frame. */
export const MAX_BATCH_ITEMS = 256;
/** Longest varint encoding: 10 groups of 7 bits covers 64. */
export const MAX_VARINT_BYTES = 10;
/** Below this, compression costs more than it saves. */
export const COMPRESS_MIN_BYTES = 512;
/** Minimum percentage saved for a compressed payload to be sent compressed. */
export const COMPRESS_MIN_GAIN_PERCENT = 10;
/** Most topics one session may subscribe to. */
export const MAX_SUBSCRIPTIONS = 512;
/** Default heartbeat interval in milliseconds. */
export const DEFAULT_HEARTBEAT_MS = 30000;
/** Frames buffered per session before it is considered lagging. */
export const SESSION_QUEUE_CAPACITY = 256;
/** How long a lagging session has to catch up before it is dropped. */
export const LAGGING_DEADLINE_MS = 5000;
/** How long a batch waits for more frames before it is flushed. */
export const BATCH_LINGER_MS = 15;
/** Frames kept for resume. */
export const RESUME_BUFFER_FRAMES = 512;
/** How long a session may be resumed after disconnect. */
export const RESUME_WINDOW_MS = 120000;
