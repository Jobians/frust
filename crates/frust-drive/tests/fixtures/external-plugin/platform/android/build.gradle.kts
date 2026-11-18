plugins {
    id("com.android.library")
}

android {
    namespace = "dev.example.extplugin"
    compileSdk = 36

    defaultConfig {
        minSdk = 29
        // Keep rules for the JNI-loaded host travel with this module.
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
