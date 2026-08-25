package com.migo.core.wire

/**
 * Protocol limits, mirrored from shared/protocol/schema/meta.json.
 *
 * These are the same numbers the Rust crate `migo-wire` and the generated `@migo/protocol`
 * carry, and `make protocol-check` keeps all three copies honest. They are the ceiling this
 * build can parse, not permission to send that much: the server also sends its own limits in
 * `Welcome`, and a client respects those when the two disagree.
 */
object Limits {
    /** Largest frame this build will encode or decode, header included. */
    const val MAX_FRAME_BYTES = 262144

    /** Longest UTF-8 string field. */
    const val MAX_STRING_BYTES = 65536

    /** Longest opaque byte field. */
    const val MAX_BYTES_LEN = 131072

    /** Most items in a `list<T>`. */
    const val MAX_LIST_ITEMS = 4096

    /** Most entries in a `map<string, T>`. */
    const val MAX_MAP_ITEMS = 1024

    /** Deepest struct nesting. Bounds recursion during decode. */
    const val MAX_NESTING_DEPTH = 16

    /** Most sub-frames in a BATCH frame. */
    const val MAX_BATCH_ITEMS = 256

    /** Longest varint encoding: 10 groups of 7 bits covers 64. */
    const val MAX_VARINT_BYTES = 10

    /** Below this, compression costs more than it saves. */
    const val COMPRESS_MIN_BYTES = 512

    /** Minimum percentage saved for a compressed payload to be sent compressed. */
    const val COMPRESS_MIN_GAIN_PERCENT = 10

    /** Most topics one session may subscribe to. */
    const val MAX_SUBSCRIPTIONS = 512

    /** Default heartbeat interval in milliseconds. */
    const val DEFAULT_HEARTBEAT_MS = 30000L

    /** Frames buffered per session before it is considered lagging. */
    const val SESSION_QUEUE_CAPACITY = 256

    /** How long a lagging session has to catch up before it is dropped. */
    const val LAGGING_DEADLINE_MS = 5000L

    /** How long a batch waits for more frames before it is flushed. */
    const val BATCH_LINGER_MS = 15L

    /** Frames kept for resume. */
    const val RESUME_BUFFER_FRAMES = 512

    /** How long a session may be resumed after disconnect. */
    const val RESUME_WINDOW_MS = 120000L
}
