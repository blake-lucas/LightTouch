#!/bin/bash
# Build an arm64-only debug APK from the live LightTouch working tree.
# Includes the guest USB bridge (usbmuxd-qemu) and the app service helper so the Store can install apps.
# usage: build-apk.sh <engine-dir containing arm64-v8a/{libqemu-arm.so,libc++_shared.so,portable-manifest.json,notices}> <out-name>
set -euo pipefail
ENGINE=$(realpath "$1"); NAME=$2
W=/home/blake/Documents/LightTouch-Development/port-workspace/builds
P=/home/blake/Documents/LightTouch-Development/perf
SRC=/home/blake/git-projects/LightTouch
OUT=$P/apk/$NAME; mkdir -p "$OUT"
export ANDROID_HOME=/home/blake/Android/Sdk JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64 CMAKE_BUILD_PARALLEL_LEVEL=12
cd "$SRC/Android"
./gradlew --offline --no-daemon --max-workers=8 \
  '-Dorg.gradle.jvmargs=-Xmx2g -XX:MaxMetaspaceSize=512m' -PnativeJobs=12 -PtargetAbis=arm64-v8a \
  -PportableDir=$SRC/Portable -PqemuSource=/home/blake/git-projects/qemu-ios \
  -PengineRoot=$ENGINE -PengineNoticeArm64=$ENGINE/arm64-v8a/notices -PfirmwareKitDir=$SRC/build/firmwarekit/android \
  -PappServiceDir=$SRC/build/appservice/android \
  -PsourceRevision=$(git -C $SRC rev-parse HEAD) -PsourceReference=https://github.com/blake-lucas/LightTouch/tree/dev/perf \
  -PbuildRoot=$P/apk/gradle-build \
  -PmobileServicesArm64=$W/mobile-android/arm64-v8a \
  -PfrontendNotices=$W/product/frontend-inputs-15 -PfrontendNoticesSha256=88918185b1a3ba3ea1b4c910c8b7dde56391597574f53824a160fef45d2b76a7 \
  -PangleArm64Libraries=$W/graphics/angle-android-arm64-gl-build-01/candidate-01/lib \
  -PangleArm64Manifest=$W/graphics/angle-android-arm64-gl-build-01/candidate-01/elf-closure.json \
  -PangleArm64Notices=$W/graphics/angle-android-arm64-gl-build-01/candidate-01/notices \
  -PangleArm64ManifestSha256=3abde58ea5d9863453b12e3d7f67eff7c96ee989fbcfa4c00a16859cfd694945 \
  -PangleArm64NoticesSha256=63d2221b0fab9bff7788dc0c4e9a94c74079b3ab5ed3c09bd3ef72c909d1f924 \
  :app:assembleDebug
cp $P/apk/gradle-build/app/outputs/apk/debug/app-debug.apk "$OUT/app-debug.apk"
echo "APK: $OUT/app-debug.apk"
