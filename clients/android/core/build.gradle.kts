// :core — the Migo SDK for Android.
//
// A pure Android library: no UI, no Activity, no Compose. Everything that has to agree with
// the Rust server and the @migo/sdk chain byte-for-byte lives here — the MWP/1 codec, the
// protocol constants, the X3DH + Double Ratchet and sender-key crypto, the OkHttp REST and
// WebSocket transport, and the Android Keystore-backed key vault.

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.serialization)
}

android {
    namespace = "com.migo.core"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
}

dependencies {
    // api, not implementation, for the three whose types appear in this module's own public
    // signatures. Gradle puts an `implementation` dependency on the compile classpath of this
    // module only, so a consumer that named `MigoClientOptions.scope` or passed its own
    // `OkHttpClient` would fail to compile with "cannot access class" — a build error about a
    // dependency the consumer never chose, which is the worst kind to debug. What leaks, leaks
    // deliberately: `CoroutineScope` on MigoClientOptions.scope, `OkHttpClient` on its
    // restClient and socketClient, and `Flow` on Settings.flow, because a caller that owns the
    // lifetime of an SDK has to be able to hand it a scope and an HTTP client it already has.
    api(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.coroutines.android)
    // REST request and response bodies are JSON — a control-plane format, explicitly not the
    // realtime wire format, which is MWP/1 binary over the WebSocket. See migo.md section 178.
    // Only the serializer runtime leaks: Grant is @Serializable and public, so a consumer that
    // stores one needs the annotation on its classpath.
    api(libs.kotlinx.serialization.json)
    api(libs.okhttp)
    // Neither of these appears in a public signature. DataStore is behind Settings, which hands
    // back a Flow of its own data class, and core-ktx is used internally only.
    implementation(libs.androidx.datastore.preferences)
    implementation(libs.androidx.core.ktx)

    // libsodium, bundled for Android as an AAR that carries the native .so for every ABI,
    // plus JNA (also as an AAR) for the JNI bridge. XChaCha20-Poly1305, X25519, Ed25519,
    // Argon2id, HKDF-SHA256 and HMAC-SHA256 all come from here — the same audited primitives
    // the @noble/* libraries wrap on the web client. No cryptography is implemented in Kotlin.
    implementation("com.goterl:lazysodium-android:5.1.0@aar")
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    testImplementation("junit:junit:4.13.2")
}
