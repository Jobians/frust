# External-plugin fixture Android module keep rules
#
# Shipped as `consumerProguardFiles` (see `build.gradle.kts`), so a consuming
# app inherits them automatically. The failure this prevents is release-only:
# a debug build never reproduces it.
#
# `dev.example.extplugin.ExtPluginHost` is never referenced from a consuming
# app's Kotlin or Java. The Rust side loads it by name through the application
# classloader and calls it over JNI. R8 sees neither, so without this rule a
# minified build strips or renames the class.
-keep class dev.example.extplugin.ExtPluginHost { *; }
