plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Where engine/build-engine.sh left the engine: libqemu-aarch64-softmmu.so and
// qemu-bundle/. -PengineDir=/path overrides it. Without an engine the APK still
// builds; the app then says so instead of starting.
val engineDir = file(providers.gradleProperty("engineDir").orElse(rootProject.file("build/engine").absolutePath).get())
val engineLib = File(engineDir, "libqemu-aarch64-softmmu.so")
val engineBundled = engineLib.isFile

android {
    namespace = "org.lighttouch.inferno"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "org.lighttouch.inferno"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
        buildConfigField("boolean", "ENGINE_BUNDLED", engineBundled.toString())
    }

    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
            // Signed with the debug key so a release build installs straight away;
            // swap in your own signingConfig to publish it.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    buildFeatures { compose = true; buildConfig = true }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    kotlinOptions { jvmTarget = "17" }
    externalNativeBuild { cmake { path = file("src/main/cpp/CMakeLists.txt"); version = "3.22.1" } }
    // Uncompressed and page-aligned in the APK, so the loader maps the engine
    // straight out of it instead of unpacking 100 MB on install.
    packaging { jniLibs { useLegacyPackaging = false } }
    testOptions { unitTests.isReturnDefaultValues = true }
}

dependencies {
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.compose.ui:ui:1.7.8")
    implementation("androidx.compose.foundation:foundation:1.7.8")
    implementation("androidx.compose.material3:material3:1.3.2")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.7")
    implementation("androidx.documentfile:documentfile:1.0.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("net.sf.kxml:kxml2:2.3.0")
}

// The engine goes in as a prebuilt JNI library, its data files as assets.
val engineJniLibs = layout.buildDirectory.dir("generated/engine/jniLibs")
val engineAssets = layout.buildDirectory.dir("generated/engine/assets")
val stageEngine = tasks.register<Sync>("stageEngine") {
    into(layout.buildDirectory.dir("generated/engine"))
    if (engineBundled) {
        from(engineLib) { into("jniLibs/arm64-v8a") }
        from(File(engineDir, "qemu-bundle/usr/local/share/qemu")) { into("assets/qemu-data") }
    }
}
android.sourceSets["main"].jniLibs.srcDir(engineJniLibs)
android.sourceSets["main"].assets.srcDir(engineAssets)
tasks.named("preBuild").configure { dependsOn(stageEngine) }
