plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "org.sinkon.launcher"
    compileSdk = 34
    ndkVersion = "26.1.10909125"

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
        ndk {
            // aarch64 is primary; armv7 and x86_64 ship from the same source tree.
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    sourceSets {
        getByName("main") {
            // `scripts/build-android.sh` drops libsinkon.so here, one directory
            // per ABI, so the module is self-contained when copied into a fork.
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    packaging {
        jniLibs {
            // Keep the .so uncompressed and page-aligned: it is loaded directly
            // by the launcher's render thread.
            useLegacyPackaging = false
        }
    }
}

dependencies {
    implementation("androidx.annotation:annotation:1.8.2")
}
