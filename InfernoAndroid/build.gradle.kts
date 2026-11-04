plugins {
    id("com.android.application") version "8.9.2" apply false
    id("org.jetbrains.kotlin.android") version "2.1.20" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.1.20" apply false
}

// build/ at the top is where engine/build-*.sh put the engine; keep `clean` out of it.
layout.buildDirectory.set(file("build/gradle-root"))
