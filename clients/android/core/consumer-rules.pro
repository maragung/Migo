# Consumer ProGuard/R8 rules for the :core SDK.
#
# These rules are packaged into the AAR and applied to any app that shrinks while
# depending on :core — the app inherits them without having to know the SDK's internals.
#
# The SDK deliberately needs almost nothing here. Its own reflection-sensitive
# dependencies ship their own consumer rules: kotlinx.serialization keeps its generated
# serializers, and Lazysodium/JNA keep the JNI-bound native method signatures. What
# remains is the SDK's own JNA surface, kept defensively so a shrunk release build cannot
# strip a native binding that is only ever reached over JNI.

# JNA maps Java methods to native symbols by name and signature, so a renamed or removed
# method breaks the binding at runtime rather than at build time. Keep the JNA runtime and
# any structure/callback types intact.
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { *; }
-dontwarn java.awt.**

# Lazysodium reaches libsodium through JNA; keep its native-facing types for the same reason.
-keep class com.goterl.lazysodium.** { *; }
