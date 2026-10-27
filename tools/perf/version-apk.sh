#!/bin/bash
# Build a released version as a side-by-side APK, for A/B regression tests between versions on one phone.
#   tools/perf/version-apk.sh VERSION [APP_REV ENGINE_REV]
# Builds tag vVERSION (engine lighttouch-vVERSION), or the given revisions, into $LT_AB_ROOT/VERSION and writes
# $LT_AB_ROOT/LightTouch-VERSION-ab.apk: non-debuggable, package org.lighttouch.android.vXYZ ("LightTouch VERSION"),
# its own data beside the regular app. Install, then measure with LT_PKG=org.lighttouch.android.vXYZ fcbench.py -.
#
# The build is laid out as build-release.sh lays out a release ($R/app beside $R/engine, objects in $R/app/build),
# because the engine's profile (PGO) names static functions by their source path relative to the object tree: the
# same source built from another directory gets a different profile match and different code.
# Guest tools and, for versions whose build.sh wants one, the engine kit come from that release's workspace
# (release-VERSION); set LT_GUEST_TOOLS / LT_ENGINE_KIT otherwise.
set -euo pipefail
V=${1:?usage: version-apk.sh VERSION [APP_REV ENGINE_REV]}
APP_REV=${2:-v$V}; ENGINE_REV=${3:-lighttouch-v$V}
DEV=$HOME/Documents/LightTouch-Development
ROOT=${LT_AB_ROOT:-$DEV/ab}; R=$ROOT/$V
APP_REPO=$(cd "$(dirname "$0")/../.." && pwd); ENGINE_REPO=${QEMU_IOS_REPO:-$HOME/git-projects/qemu-ios}
REL=$DEV/release-$V
SUFFIX=.v${V//./}
export PATH=$HOME/.cargo/bin:$PATH

mkdir -p "$R"
[ -d "$R/app" ] || git -C "$APP_REPO" worktree add --detach "$R/app" "$APP_REV"
[ -d "$R/engine" ] || git -C "$ENGINE_REPO" worktree add --detach "$R/engine" "$ENGINE_REV"
# Subprojects are gitignored: take them from that release's engine (patched as it shipped), else the live engine.
for p in berkeley-softfloat-3 berkeley-testfloat-3 keycodemapdb dtc slirp; do
    [ -d "$R/engine/subprojects/$p" ] && continue
    for src in "$REL/engine/subprojects/$p" "$ENGINE_REPO/subprojects/$p"; do
        if [ -d "$src" ]; then cp -a "$src" "$R/engine/subprojects/"; break; fi
    done
done

env=(QEMU_IOS="$R/engine" LT_DEBUGGABLE=false)
tools=${LT_GUEST_TOOLS:-$REL/app/build/firmware-tools}
[ -f "$tools/armv7.itpack" ] || { echo "no guest tools in $tools: set LT_GUEST_TOOLS" >&2; exit 1; }
env+=(LT_GUEST_TOOLS="$tools")
if grep -q LT_ENGINE_KIT "$R/app/scripts/build.sh"; then
    kit=${LT_ENGINE_KIT:-$REL/app/build/android/engine/arm64-v8a}
    [ -d "$kit/notices" ] || { echo "no engine kit in $kit: set LT_ENGINE_KIT" >&2; exit 1; }
    env+=(LT_ENGINE_KIT="$kit")
fi
if grep -q LT_APP_ID_SUFFIX "$R/app/scripts/build.sh"; then
    env+=(LT_APP_ID_SUFFIX="$SUFFIX" LT_APP_LABEL="LightTouch $V")
else
    # Older versions have no side-by-side option: rename the package in the worktree.
    sed -i "s/applicationId = \"org.lighttouch.android\"\$/applicationId = \"org.lighttouch.android$SUFFIX\"/" \
        "$R/app/Android/app/build.gradle.kts"
    sed -i "s/<application android:label=\"LightTouch\"/<application android:label=\"LightTouch $V\"/" \
        "$R/app/Android/app/src/main/AndroidManifest.xml"
    grep -q "org.lighttouch.android$SUFFIX" "$R/app/Android/app/build.gradle.kts" ||
        { echo "could not rename the package in $R/app" >&2; exit 1; }
fi
(cd "$R/app" && env "${env[@]}" bash scripts/build.sh android > "$R/build.log" 2>&1) ||
    { tail -20 "$R/build.log" >&2; exit 1; }
cp "$R/app/build/android/LightTouch-debug.apk" "$ROOT/LightTouch-$V-ab.apk"
echo "$ROOT/LightTouch-$V-ab.apk (org.lighttouch.android$SUFFIX)"
