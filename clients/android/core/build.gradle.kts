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
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.coroutines.android)
    // REST request and response bodies are JSON — a control-plane format, explicitly not the
    // realtime wire format, which is MWP/1 binary over the WebSocket. See migo.md section 178.
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.okhttp)
    implementation(libs.androidx.core.ktx)

    // libsodium, bundled for Android as an AAR that carries the native .so for every ABI,
    // plus JNA (also as an AAR) for the JNI bridge. XChaCha20-Poly1305, X25519, Ed25519,
    // Argon2id, HKDF-SHA256 and HMAC-SHA256 all come from here — the same audited primitives
    // the @noble/* libraries wrap on the web client. No cryptography is implemented in Kotlin.
    implementation("com.goterl:lazysodium-android:5.1.0@aar")
    implementation("net.java.dev.jna:jna:5.14.0@aar")

    testImplementation("junit:junit:4.13.2")
}
