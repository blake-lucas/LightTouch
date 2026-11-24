#!/bin/bash
# LT Bench on an Android phone (docs/GUEST-BENCHMARKS.md). ANDROID_SERIAL set; the guest running
# with the USB bridge (any APK that bundles libusbmuxd_qemu.so).
#   android.sh install IPA [BUNDLE]  install LTBench.ipa (or another app, by bundle id) into the running guest
#   android.sh results [N]     the [ltbench] lines of the newest N passes (default 1) in the engine log
# Launch it by tapping its "LT Bench" icon; it runs once at launch and again on each tap, and
# shows the best ns/op of each test on screen when done.
set -euo pipefail
PKG=${LT_PKG:-org.lighttouch.android}
as() { adb shell "run-as $PKG sh -c '$1'"; }
case "${1:-}" in
install)
    lib=$(adb shell dumpsys package $PKG | sed -n 's/.*legacyNativeLibraryDir=//p' | tr -d '\r')/arm64
    sock=$(as 'ls -t files/usb-run/*/mux.sock 2>/dev/null | head -1' | tr -d '\r')
    [ -n "$sock" ] || { echo "no guest USB bridge running: start the guest with an APK that bundles it" >&2; exit 1; }
    adb push -q "$2" /data/local/tmp/LTBench.ipa
    as "cp /data/local/tmp/LTBench.ipa files/LTBench.ipa"
    adb shell rm /data/local/tmp/LTBench.ipa
    as "LT_UPLOAD_AFC=${LT_UPLOAD_AFC:-} $lib/liblt_appservice.so install --socket /data/data/$PKG/$sock --ipa /data/data/$PKG/files/LTBench.ipa --bundle-id ${3:-org.lighttouch.ltbench}"
    ;;
results)
    as "cat files/logs/engine.log files/logs/engine.tail.1.log files/logs/engine.tail.log 2>/dev/null | grep -a ltbench" | tr -d '\r' | awk -v n="${2:-1}" '
        /\[ltbench\] start pass/ { buf[++k] = "" } k { buf[k] = buf[k] $0 "\n" }
        END { for (i = (k > n ? k - n + 1 : 1); i <= k; i++) printf "%s", buf[i] }'
    ;;
*) sed -n 2,7p "$0"; exit 2 ;;
esac
