import java.security.MessageDigest

plugins { id("com.android.application"); id("org.jetbrains.kotlin.android"); id("org.jetbrains.kotlin.plugin.compose") }
val shutdownPolicyHeader = file(providers.gradleProperty("portableDir").orElse(file("../../Portable").absolutePath).get() + "/include/lighttouch.h")
val shutdownGraceMs = Regex("(?m)^#define\\s+LT_SHUTDOWN_GRACE_MS\\s+([0-9]+)u?\\s*$")
    .find(shutdownPolicyHeader.readText())?.groupValues?.get(1)?.toLong()
    ?: error("Shared LT_SHUTDOWN_GRACE_MS policy is unavailable")
require(shutdownGraceMs > 0)
android {
    namespace = "org.lighttouch.android"
    compileSdk = 35
    buildToolsVersion = "35.0.0"
    ndkVersion = "28.2.13676358"
    defaultConfig {
        // -PappIdSuffix=.name installs a test build beside the regular app (its own data); -PappLabel names it.
        applicationId = "org.lighttouch.android" + providers.gradleProperty("appIdSuffix").orElse("").get()
        minSdk = 29
        targetSdk = 35
        versionCode = 12
        versionName = "0.0.12"
        buildConfigField("long", "SHUTDOWN_GRACE_MS", "${shutdownGraceMs}L")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk { abiFilters += providers.gradleProperty("targetAbis").orElse("arm64-v8a,x86_64").get().split(",") }
        externalNativeBuild { cmake { arguments += listOf("-DLT_NATIVE_JOBS=" + providers.gradleProperty("nativeJobs").orElse("2").get(), "-DANDROID_STL=c++_shared", "-DLT_PORTABLE_DIR=" + providers.gradleProperty("portableDir").orElse(file("../../Portable").absolutePath).get(), "-DLT_ENGINE_ROOT=" + providers.gradleProperty("engineRoot").orElse("").get(), "-DLIGHTTOUCH_QEMU_SOURCE=" + providers.gradleProperty("qemuSource").orElse("").get(), "-DOPENSSL_ROOT_DIR=" + providers.gradleProperty("networkDeps").orElse("").get()) } }
    }
    // -Pdebuggable=false makes a debug APK the runtime may compile ahead of time. Phones with the
    // JIT off (GrapheneOS) otherwise interpret a debuggable app's whole UI.
    buildTypes { getByName("debug") { isDebuggable = providers.gradleProperty("debuggable").orElse("true").get().toBoolean() } }
    // -Pprofileable=true lets simpleperf attach from adb shell to a non-debuggable build (perf runs only).
    defaultConfig { manifestPlaceholders["profileable"] = providers.gradleProperty("profileable").orElse("false").get() }
    defaultConfig { manifestPlaceholders["appLabel"] = providers.gradleProperty("appLabel").orElse("LightTouch").get() }
    buildFeatures { compose = true; aidl = true; buildConfig = true }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
    kotlinOptions { jvmTarget = "17" }
    externalNativeBuild { cmake { path = file("src/main/cpp/CMakeLists.txt"); version = "3.22.1" } }
    // Guest app-services executables and firmwarekit run from nativeLibraryDir, which holds files only when extracted.
    packaging { jniLibs { useLegacyPackaging = providers.gradleProperty("mobileServicesArm64").isPresent || providers.gradleProperty("firmwareKitDir").isPresent } }
}
dependencies {
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.compose.ui:ui:1.7.8")
    implementation("androidx.compose.foundation:foundation:1.7.8")
    implementation("androidx.compose.material3:material3:1.3.2")
    implementation("androidx.documentfile:documentfile:1.0.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    testImplementation("junit:junit:4.13.2")
}

providers.gradleProperty("buildRoot").orNull?.let { output ->
    layout.buildDirectory.set(file("$output/app"))
    android.externalNativeBuild.cmake.buildStagingDirectory = file("$output/native")
}

val catalogAssets = layout.buildDirectory.dir("generated/catalogAssets")
val prepareCatalog = tasks.register<Copy>("prepareFirmwareCatalog") {
    from(file("../../Portable/firmware/firmware-catalog.json"))
    into(catalogAssets)
}
android.sourceSets["main"].assets.srcDir(catalogAssets)
tasks.named("preBuild").configure { dependsOn(prepareCatalog) }

// The shared network backend links the same pinned OpenSSL producer prefix as
// the engine dependencies; include its own notice even when the engine uses gcrypt.
val networkNoticeAssets = layout.buildDirectory.dir("generated/networkNoticeAssets")
val prepareNetworkNotices = tasks.register<Exec>("prepareNetworkNotices") {
    val prefix = providers.gradleProperty("networkDeps")
    val source = providers.gradleProperty("qemuSource")
    inputs.file(rootProject.file("scripts/stage-network-notices.py"))
    inputs.property("prefix", prefix.orElse(""))
    inputs.files(prefix.map { file("$it/lib/libssl.a") }, prefix.map { file("$it/lib/libcrypto.a") })
    inputs.file(source.map { file("$it/contrib/portable/dependencies.json") })
    outputs.dir(networkNoticeAssets)
    doFirst {
        commandLine("python3", rootProject.file("scripts/stage-network-notices.py").absolutePath,
            networkNoticeAssets.get().dir("licenses/network").asFile.absolutePath,
            "--prefix", prefix.get(), "--pins", source.get() + "/contrib/portable/dependencies.json")
    }
}
android.sourceSets["main"].assets.srcDir(networkNoticeAssets)
tasks.named("preBuild").configure { dependsOn(prepareNetworkNotices) }

// Guest tools (-PguestToolsDir: scripts/build.sh firmware-tools, qemu-ios contrib/export-guest-artifacts.sh's
// ipad-guest-tools). New devices get the armv6/armv7.itpack whose families cover their board and build; firmwarekit
// bakes the helpers beside them into each system it prepares.
providers.gradleProperty("guestToolsDir").orNull?.let { input ->
    val packs = file(input)
    require(listOf("armv6.itpack", "armv7.itpack", "it_seal", "ittrust", "itproxy").all { packs.resolve(it).isFile }) { "guestToolsDir must hold the guest packages and network helpers (scripts/build.sh firmware-tools)" }
    val guestToolAssets = layout.buildDirectory.dir("generated/guestToolAssets")
    val stage = tasks.register<Copy>("prepareGuestTools") {
        from(packs)
        into(guestToolAssets.map { it.dir("guest-tools") })
    }
    android.sourceSets["main"].assets.srcDir(guestToolAssets)
    tasks.named("preBuild").configure { dependsOn(stage) }
}

// Optional private usbmuxd-qemu daemon and IPA helper (Android/packaging/build-mobile-android.py OUTPUT/arm64-v8a).
providers.gradleProperty("mobileServicesArm64").orNull?.let { input ->
    val executables = listOf("libusbmuxd_qemu.so", "liblt_appservice.so")
    require(executables.all { file("$input/$it").isFile }) { "mobileServicesArm64 must contain ${executables.joinToString()}" }
    require("arm64-v8a" in providers.gradleProperty("targetAbis").orElse("arm64-v8a,x86_64").get().split(",")) {
        "mobileServicesArm64 requires arm64-v8a in targetAbis"
    }
    val mobileServicesJniLibs = layout.buildDirectory.dir("generated/mobileServicesJniLibs")
    val prepareMobileServices = tasks.register<Copy>("prepareMobileServices") {
        from(file(input)) { include(executables) }
        into(mobileServicesJniLibs.map { it.dir("arm64-v8a") })
    }
    android.sourceSets["main"].jniLibs.srcDir(mobileServicesJniLibs)
    tasks.named("preBuild").configure { dependsOn(prepareMobileServices) }
}

// The publisher can override these for its complete corresponding-source bundle.
// The local checkout reference is development provenance, not distribution clearance.
val sourceRevision = providers.gradleProperty("sourceRevision").orElse(
    providers.exec { commandLine("git", "-C", rootProject.projectDir.parentFile.absolutePath, "rev-parse", "HEAD") }
        .standardOutput.asText.map { it.trim() }
)
val sourceReference = providers.gradleProperty("sourceReference").orElse(
    sourceRevision.map { "https://github.com/blake-lucas/LightTouch/tree/$it" }
)
// firmwarekit, the preparer every port shares (-PfirmwareKitDir: scripts/build.sh firmwarekit-android): it ships as an
// executable in nativeLibraryDir, with the notices of what it links. Its preparation boots load the APK's engine
// themselves (`firmwarekit oneshot`), so no separate helper ships.
providers.gradleProperty("firmwareKitDir").orNull?.let { input ->
    val kit = file(input)
    require(listOf("firmwarekit", "licenses/manifest.json").all { kit.resolve(it).isFile }) {
        "firmwareKitDir must hold firmwarekit and licenses/ (scripts/build.sh firmwarekit-android)"
    }
    val firmwareKitJniLibs = layout.buildDirectory.dir("generated/firmwareKitJniLibs")
    val firmwareKitNotices = layout.buildDirectory.dir("generated/firmwareKitNotices")
    val stage = tasks.register<Copy>("prepareFirmwareKit") {
        from(kit) { include("firmwarekit"); rename { "libfirmwarekit.so" } }
        into(firmwareKitJniLibs.map { it.dir("arm64-v8a") })
    }
    val notices = tasks.register<Copy>("prepareFirmwareKitNotices") {
        from(kit.resolve("licenses"))
        into(firmwareKitNotices.map { it.dir("licenses/firmwarekit") })
    }
    android.sourceSets["main"].jniLibs.srcDir(firmwareKitJniLibs)
    android.sourceSets["main"].assets.srcDir(firmwareKitNotices)
    tasks.named("preBuild").configure { dependsOn(stage, notices) }
}

// Consume backend-owned, audited bundles; do not regenerate dependency notices here.
val engineNoticeInputs = mapOf(
    "arm64-v8a" to providers.gradleProperty("engineNoticeArm64"),
    "x86_64" to providers.gradleProperty("engineNoticeX86")
)
val engineNoticeAssets = layout.buildDirectory.dir("generated/engineNoticeAssets")
val prepareEngineNotices = tasks.register<Exec>("prepareEngineNotices") {
    val selectedAbis = providers.gradleProperty("targetAbis").orElse("arm64-v8a,x86_64").get().split(",")
    val supplied = selectedAbis.filter { engineNoticeInputs.getValue(it).isPresent }
    require(supplied.isEmpty() || supplied.size == selectedAbis.size) {
        "Provide an engine notice bundle for every packaged ABI"
    }
    val engineRoot = providers.gradleProperty("engineRoot").orElse("")
    inputs.file(rootProject.file("scripts/stage-engine-notices.py"))
    inputs.property("selectedAbis", selectedAbis)
    supplied.forEach { abi ->
        inputs.dir(engineNoticeInputs.getValue(abi).map { file(it) })
        inputs.file(engineRoot.map { file("$it/$abi/libqemu-arm.so") })
        inputs.file(engineRoot.map { file("$it/$abi/libc++_shared.so") })
    }
    outputs.dir(engineNoticeAssets)
    doFirst {
        val arguments = mutableListOf("python3", rootProject.file("scripts/stage-engine-notices.py").absolutePath,
            engineNoticeAssets.get().dir("licenses").asFile.absolutePath, "--engine-root", engineRoot.get())
        supplied.forEach { abi -> arguments += listOf("--bundle", abi, engineNoticeInputs.getValue(abi).get()) }
        commandLine(arguments)
    }
}
android.sourceSets["main"].assets.srcDir(engineNoticeAssets)
tasks.named("preBuild").configure { dependsOn(prepareEngineNotices) }

// Explicit audit only: resolve the selected runtime graph without compiling or
// confusing build-tool/androidTest dependencies with the conveyed main APK.
tasks.register("recordApkRuntimeArtifacts") {
    val configurationName = providers.gradleProperty("auditConfiguration").orElse("debugRuntimeClasspath")
    val report = layout.buildDirectory.file(configurationName.map { "reports/$it-artifacts.json" })
    outputs.file(report)
    outputs.upToDateWhen { false }
    doLast {
        require(configurationName.get() in listOf("debugRuntimeClasspath", "releaseRuntimeClasspath"))
        val artifacts = configurations.getByName(configurationName.get()).resolvedConfiguration.resolvedArtifacts
            .sortedBy { "${it.moduleVersion.id}:${it.name}:${it.classifier ?: ""}" }
            .map { artifact ->
                val digest = MessageDigest.getInstance("SHA-256")
                artifact.file.inputStream().use { input ->
                    val block = ByteArray(1024 * 1024)
                    while (true) { val size = input.read(block); if (size < 0) break; digest.update(block, 0, size) }
                }
                mapOf("coordinate" to artifact.moduleVersion.id.toString(), "file" to artifact.file.name,
                    "sha256" to digest.digest().joinToString("") { "%02x".format(it) },
                    "artifact_path" to artifact.file.absolutePath)
            }
        val document = mapOf("schema" to 1, "configuration" to configurationName.get(), "artifacts" to artifacts,
            "scope" to "selected main APK runtime inputs; private audit paths are not offline assets or source distribution clearance")
        val output = report.get().asFile
        output.parentFile.mkdirs()
        output.writeText(groovy.json.JsonOutput.prettyPrint(groovy.json.JsonOutput.toJson(document)) + "\n")
        println("Recorded ${artifacts.size} selected runtime artifacts: ${output.name}")
    }
}

// The optional provider is staged only in debug APKs, never linked directly or
// included in release. Its selection remains a closed worker-only debug enum.
// Legacy names continue to select x86; ARM64 has independent mandatory pins.
val angleInputs = mapOf(
    "x86_64" to listOf("angleLibraries", "angleManifest", "angleNotices", "angleManifestSha256", "angleNoticesSha256"),
    "arm64-v8a" to listOf("angleArm64Libraries", "angleArm64Manifest", "angleArm64Notices", "angleArm64ManifestSha256", "angleArm64NoticesSha256")
).mapValues { (_, names) -> names.map { providers.gradleProperty(it) } }
val angleStage = layout.buildDirectory.dir("generated/angleRuntime")
val prepareAngleRuntime = tasks.register<Exec>("prepareAngleRuntime") {
    val packagedAbis = providers.gradleProperty("targetAbis").orElse("arm64-v8a,x86_64").get().split(",")
    angleInputs.forEach { (abi, properties) ->
        require(properties.all { it.isPresent } || properties.none { it.isPresent }) {
            "Supply all five ANGLE runtime/closure/notice inputs and pins for $abi"
        }
    }
    val selected = angleInputs.filterValues { properties -> properties.all { it.isPresent } }
    require(selected.keys.all { it in packagedAbis }) { "ANGLE provider ABI must be packaged in targetAbis" }
    if ("arm64-v8a" in selected) {
        require((android.defaultConfig.minSdk ?: 0) >= 29) { "The fixed ARM64 ANGLE provider requires minSdk >= 29" }
    }
    inputs.file(rootProject.file("scripts/stage-angle-runtime.py"))
    inputs.property("angleABIs", selected.keys.sorted())
    selected.forEach { (abi, properties) ->
        inputs.dir(properties[0].map { file(it) })
        inputs.file(properties[1].map { file(it) })
        inputs.dir(properties[2].map { file(it) })
        inputs.property("angleManifestSha-$abi", properties[3])
        inputs.property("angleNoticesSha-$abi", properties[4])
    }
    outputs.dir(angleStage)
    doFirst {
        val arguments = mutableListOf("python3", rootProject.file("scripts/stage-angle-runtime.py").absolutePath, angleStage.get().asFile.absolutePath)
        selected.forEach { (abi, properties) ->
            arguments += listOf("--bundle", abi) + properties.map { it.get() }
        }
        if (selected.isNotEmpty()) {
            arguments += listOf("--readelf", androidComponents.sdkComponents.ndkDirectory.get().file("toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf").asFile.absolutePath)
        }
        commandLine(arguments)
    }
}
android.sourceSets["debug"].jniLibs.srcDir(angleStage.map { it.dir("jniLibs") })
android.sourceSets["debug"].assets.srcDir(angleStage.map { it.dir("assets") })
android.packaging.jniLibs.keepDebugSymbols += setOf("**/libEGL_lighttouch_angle.so", "**/libGLESv1_CM_lighttouch_angle.so", "**/libGLESv2_lighttouch_angle.so")
// AGP registers variant tasks after this script is evaluated.
tasks.matching { it.name == "preDebugBuild" }.configureEach { dependsOn(prepareAngleRuntime) }

// Firmware owns extraction; this consumer verifies the complete unchanged bundle
// against the actual main variant graph before copying public notice assets.
val frontendNotices = providers.gradleProperty("frontendNotices")
val frontendNoticesSha = providers.gradleProperty("frontendNoticesSha256")
val frontendNoticeAssets = layout.buildDirectory.dir("generated/frontendNoticeAssets")
val prepareFrontendNotices = tasks.register<Exec>("prepareFrontendNotices") {
    require(frontendNotices.isPresent == frontendNoticesSha.isPresent) {
        "Supply frontendNotices and its frontendNoticesSha256 pin together"
    }
    inputs.file(rootProject.file("scripts/stage-frontend-notices.py"))
    inputs.property("frontendEnabled", frontendNotices.isPresent)
    if (frontendNotices.isPresent) {
        dependsOn("recordApkRuntimeArtifacts")
        inputs.dir(frontendNotices.map { file(it) }); inputs.property("frontendManifestPin", frontendNoticesSha)
        inputs.file(layout.buildDirectory.file(providers.gradleProperty("auditConfiguration").orElse("debugRuntimeClasspath")
            .map { "reports/$it-artifacts.json" }))
    }
    outputs.dir(frontendNoticeAssets)
    doFirst {
        val arguments = mutableListOf("python3", rootProject.file("scripts/stage-frontend-notices.py").absolutePath,
            frontendNoticeAssets.get().dir("licenses/frontend").asFile.absolutePath)
        if (frontendNotices.isPresent) {
            arguments += listOf("--bundle", frontendNotices.get(), "--manifest-sha256", frontendNoticesSha.get(),
                "--inventory", layout.buildDirectory.file("reports/" + providers.gradleProperty("auditConfiguration").orElse("debugRuntimeClasspath").get() + "-artifacts.json").get().asFile.absolutePath)
        }
        commandLine(arguments)
    }
}
android.sourceSets["main"].assets.srcDir(frontendNoticeAssets)
tasks.named("preBuild").configure { dependsOn(prepareFrontendNotices) }
