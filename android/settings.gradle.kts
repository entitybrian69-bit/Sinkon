// Gradle settings for the standalone Sinkon Android module.
//
// The module is published as `:sinkon-native` because that is the name the
// MojoLauncher fork patch expects (`include ':sinkon-native'`). Keeping the
// directory named `app` while the Gradle project is `sinkon-native` means the
// same tree can be dropped into a launcher fork without renaming folders.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "sinkon-android"

include(":sinkon-native")
project(":sinkon-native").projectDir = file("app")

// Debug harness app: builds a small APK that drives libsinkon.so on a
// SurfaceView, so the pipeline can be smoke-tested without a launcher fork.
include(":sample")
