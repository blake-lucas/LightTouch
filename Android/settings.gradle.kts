// LT_MAVEN_MIRROR (CI sets it to Google's mirror of Maven Central) is tried before Central itself, which rate-limits
// shared runners. It stays after google(): some Android artifacts exist on both with different bytes (transform-api),
// and verification-metadata.xml pins Google's. Verification checks every artifact's hash, whichever server sent it.
pluginManagement { repositories { google(); System.getenv("LT_MAVEN_MIRROR")?.let { maven(it) }; mavenCentral(); gradlePluginPortal() } }
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories { google(); System.getenv("LT_MAVEN_MIRROR")?.let { maven(it) }; mavenCentral() }
}
rootProject.name = "LightTouchAndroid"
include(":app")
