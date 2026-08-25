// Root build script. Declares the plugins the modules use, but applies none of them here —
// each module applies the ones it needs. `apply false` makes the plugin available on the
// classpath with its version pinned by the catalog, so a module can `alias(...)` it without
// repeating the version.

plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.android) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.compose.compiler) apply false
}
