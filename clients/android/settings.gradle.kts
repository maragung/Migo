// Migo Android — Gradle settings.
//
// Two modules, mirroring the TypeScript SDK/client split:
//   :core  the SDK — wire codec, protocol constants, crypto, transport, client, storage.
//          Depends on no Android UI. Everything that must agree byte-for-byte with the
//          Rust server and the @migo/sdk chain lives here and is exercised by the shared
//          conformance vectors under shared/protocol/vectors.
//   :app   the Jetpack Compose UI over :core. Knows nothing about the wire format.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    // A project that declares its own repository is almost always a mistake — a stray
    // repo is how an unreviewed artefact enters the build — so fail rather than let one
    // through. Every dependency resolves against exactly these two registries.
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "migo-android"

include(":core", ":app")
