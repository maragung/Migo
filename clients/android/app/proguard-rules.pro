# App-level ProGuard/R8 rules.
#
# Empty on purpose. The release build type sets isMinifyEnabled = false (see
# app/build.gradle.kts): there is no release-signing config in the tree, so CI assembles
# the debug variant and R8 never runs. This file exists because the build script names it,
# and is the place to add keep rules the day a signing key lives outside the repository and
# minification is turned on. The SDK's own consumer rules (see core/consumer-rules.pro)
# already cover the JNI-bound crypto surface, so most apps will need nothing beyond this.
