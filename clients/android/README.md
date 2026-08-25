# Migo for Android

A native Android client for Migo, built on the same protocol as the web client and the
Rust server. This module is the third implementation of the Migo Wire Protocol (MWP/1):
the Rust crate `migo-wire` is the first, `@migo/wire` (TypeScript) is the second, and the
Kotlin `wire` package here is the third. All three are held to the byte-exact conformance
vectors in `shared/protocol/vectors`.

## Layout

Two Gradle modules, mirroring the SDK/client split on the web side:

    core/   the SDK. No UI. Sub-packages:
              wire/       MWP/1 framing and Migo Struct Encoding (MSE)
              protocol/   generated opcode, error, and enum constants
              crypto/     X3DH, Double Ratchet, sender-key groups, sealed envelopes
              transport/  OkHttp REST client and WebSocket gateway
              client/     MigoClient and its per-feature domains
              storage/    Android Keystore-backed key vault
    app/    the Jetpack Compose UI over :core

## Cryptography

Every primitive comes from **libsodium**, via the Lazysodium-Android binding, and nothing
is implemented in Kotlin. XChaCha20-Poly1305, X25519, Ed25519, Argon2id (`crypto_pwhash`),
HKDF-SHA256 and HMAC-SHA256 are the same audited primitives the web client reaches through
`@noble/*`, chosen so the two clients produce identical bytes for the same inputs. Private
keys are generated on the device and never sent to the server; the key vault encrypts its
snapshot with an AES key held in the Android Keystore (hardware-backed where the device
offers it), so no private key is ever written to disk in the clear. This is the Android
counterpart of the web client keeping its KeyStore snapshot in IndexedDB rather than
`localStorage`.

## Transport

REST for the request/response control plane (register, sign-in, key publish, media grants)
and a single WebSocket carrying MWP/1 binary frames for everything realtime. The realtime
channel is never JSON, never long-polling, and never a timer — see `migo.md` section 178.

## Building

The sandbox this was written in has no JVM or Android toolchain, so the Kotlin here is
compiled and tested by GitHub Actions rather than locally — see `.github/workflows/android.yml`.
The CI job provisions a JDK, the Android SDK, and a pinned Gradle, then assembles both
modules and runs the unit tests, including the shared conformance vectors on the Kotlin
side. To build on a workstation that does have the toolchain:

    # once, to create the Gradle wrapper (the wrapper jar is intentionally not committed):
    cd clients/android && gradle wrapper --gradle-version 8.11.1

    ./gradlew :core:assembleDebug :app:assembleDebug
    ./gradlew :core:testDebugUnitTest

Until that CI job has run green, treat this module as written-but-unverified: the code is
complete and internally consistent, but its agreement with the vectors and its Kotlin
compilation have not yet been machine-checked.
