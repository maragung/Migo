// :app — the Jetpack Compose UI over :core.
//
// This module holds no protocol knowledge. It talks to MigoClient and renders what comes
// back; if the wire format changed, nothing here would.

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.compose.compiler)
}

android {
    namespace = "com.migo.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.migo.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildFeatures {
        compose = true
        buildConfig = true
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
            // Left off for now: there is no release-signing config in the repository, and a
            // shrunk unsigned build proves nothing CI needs. CI assembles the debug variant;
            // enabling R8 is a task for when a signing key exists outside the tree.
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }
}

dependencies {
    implementation(project(":core"))

    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.datastore.preferences)

    // The WebRTC engine of the voice call: SDP and ICE flow through :core as sealed opaque blobs,
    // and this is the device-side half that produces and consumes them -- the peer connection,
    // the microphone, the speaker. Declared here rather than in :core on purpose: core is pure
    // signaling and holds no media engine, the same split the web client keeps between its SDK
    // and its browser.
    implementation(libs.stream.webrtc)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.foundation)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)

    debugImplementation(libs.androidx.compose.ui.tooling)
}
