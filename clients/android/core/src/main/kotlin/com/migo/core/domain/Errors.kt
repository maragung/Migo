package com.migo.core.domain

/**
 * The domain layer's own failure type.
 *
 * Not a replacement for the two transport error vocabularies -- [com.migo.core.net.RestError] for the
 * HTTP bootstrap and [com.migo.core.net.GatewayError] for the socket -- but the third thing neither of
 * them can express: the server answered, the frame parsed, and what came back still does not satisfy
 * the contract the caller was promised. The reference names the same type `SdkError`
 * (`packages/sdk/src/errors.ts`) and this is the Kotlin counterpart of it.
 *
 * # Why this is not a [com.migo.core.net.GatewayError]
 *
 * `GatewayError.Malformed` means "these bytes are not the struct they claim to be", and a caller that
 * sees it reconnects, because a peer speaking a different codec will not do better on the next frame.
 * A `KEY_BUNDLE_FETCH` that returns bundles for two devices, neither of them the one that was asked
 * for, is a different situation: the codec agreed, the connection is healthy, and the right response
 * is to fail that one operation. Folding it into `Malformed` would make a routine directory miss look
 * like a broken connection and take the socket down with it.
 *
 * # What never goes in the message
 *
 * Ids and counts, yes -- a message that says which device had no bundle is what makes the failure
 * actionable. Never a byte of an envelope, a key, or a token (brief section 174). These strings reach
 * logs and crash reporters.
 */
open class SdkError(message: String) : Exception(message)
