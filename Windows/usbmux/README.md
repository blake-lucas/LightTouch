# Windows USB bridge

`lighttouch-usbmuxd.exe`, the Windows build of the private usbmuxd-qemu daemon, and its owned libusbmuxd: the
bridge between the engine and the desktop app's services (crates/lighttouch-services). The app starts it as a
child, binds readiness to that child and stops it through its stdin. The release packages the built `bin`
directory (the `services-mobile01` input of scripts/build-release.sh). The notes below record how it was built.

The peer/connector C files cross-compile to Win64 objects with GCC13 and
`-Wall -Wextra -Werror` (source b36e542e). All eight daemon C translation units
also cross-compile with real pinned libplist/glue public headers (source
95795f20, implicit-function and incompatible-pointer diagnostics treated as
errors). Upstream Windows format/unused-variable warnings remain recorded.
An actual offline MinGW build linked both the daemon and owned libusbmuxd from
source95075501 against cross-built libplist2.7/glue1.3.2 DLL providers. All five
Win64 runtime images have a closed import graph except Windows system DLLs;
the connector's 21 exports match its public header, and nine notice files are
retained. The original failed attempt (non-libtool connector `.o` objects) is
preserved; the successful recipe uses generated `.lo` compile mode. Nothing
has been executed on Windows or enabled by WindowsHost. The remaining
libimobiledevice/TLS stack and native endpoint/services acceptance are open.
This directory changes only the native process/socket boundary and validates
readiness against an already owned child. It is not a replacement installer.

## Native source handoff

`SOURCES.json` pins daemon/libusbmuxd preimages and the audited Windows reference,
including its license. Apply `windows-host.patch` to **unmodified** usbmuxd
`5cb90897479b9021a69db9698fea071ef7b2eceb`, and `libusbmuxd-owned.patch` to
unmodified libusbmuxd 2.1.1. Do not layer the reference patch or Linux patches on
top. Retain the pinned native protocol state machines. Build only the QEMU
transport, never the physical USB backend, upstream system main or service units.

For the daemon, compile the translation units named in `SOURCES.json` with
`windows-compat.h` force-included, `WIN32` and `_WIN32` defined, and this directory
on the include path. Add `windows-main.c` and `windows-peer.c`. Link the listed
WinSock/IP Helper/security/Shell libraries and pinned libplist/glue. Their build
configuration and generated headers still need a reviewed finite build packet;
the source list is **not** a successful build recipe.
Include upstream `preflight.c` without `HAVE_LIBIMOBILEDEVICE`: its established
no-preflight path publishes the QEMU device without adding another dependency
on libimobiledevice or its installer. `conf.c` also references `CoTaskMemFree`,
so the daemon link needs `ole32` in addition to `shell32`.

`build-native.py` is the offline build entrypoint for that transport milestone.
It checks the actual dependency build receipt and every installed provider hash,
checks the pinned unmodified daemon source and libusbmuxd release archive, applies
the two patches in fresh output, compiles/links the daemon and owned connector,
and collects their PE import closure and notices. It invokes no Windows binary
and does not acquire or build the rest of the mobile/TLS stack. The allocated
offline transport build passed with this entrypoint:

```sh
python3 Windows/usbmux/build-native.py \
  --deps-packet ../../builds/windows-native/services-mobile-deps01 \
  --daemon-source ../../builds/linux-desktop/dependencies/usbmuxd-source \
  --libusbmuxd-archive ../../builds/linux-desktop/bullseye-build01/mobile/sources/libusbmuxd-2.1.1.tar.bz2 \
  --output ../../builds/windows-native/services-linked-new
```

Run from this source worktree's repository root, under the integrator's exclusive
bounded build allocation. The output directory must not already exist. This
recipe rebuilds the eight small daemon units so changed native adapters cannot
silently link against stale objects. The connector uses upstream configure and
its generated configuration, with only its native adapter objects and Windows
link libraries added. No autotools regeneration or host mobile-library fallback
is used. The resulting `bin` directory is a transport test candidate; it does
not enable Apps or replace a qualified desktop package.

Every Windows daemon socket creation and accept explicitly uses `ltw_socket`
or `ltw_accept`. These retain native SOCKET width until checking `INT_MAX`,
close unrepresentable handles and disable inheritance before the existing
protocol's int-descriptor model sees them. Compatibility macros also protect
other original call sites; explicit calls do not depend on those macros for
the creation/accept conversion.

For libusbmuxd, apply the connector patch and compile `windows-connect.c` and
`windows-peer.c` into that DLL with the stated libraries. Do **not** force-include
the daemon compatibility header into libusbmuxd. The Windows connection branch
always requires the exact admitted `USBMUXD_SOCKET_ADDRESS=127.0.0.1:<port>`,
`LTW_USBMUX_PID` and decimal `LTW_USBMUX_CREATED` FILETIME; missing/malformed values
return an error before connection. It never falls back to localhost:27015.
The existing POSIX branch retains its original behavior.

## Candidate endpoint boundary

The adapter uses native reverse established TCP ownership rows and a held peer
process handle, checks both loopback endpoints and the current-user SID, and
rechecks the owner table/liveness before accepting. The library connector also
requires the exact owned daemon PID and creation time. Ambiguous rows, access
denied, races and unknown identities fail closed before protocol bytes. This is
a **candidate** kernel-ownership boundary, different from the authentication
preface previously proposed in SERVICES.md. Actual Windows foreign-user,
socket-churn, PID-reuse and multi-device tests are mandatory before enabling it.
Same-user processes have the same trust as an owner-only POSIX socket; this does
not authenticate a particular guest or resist a malicious process of that user.

Listeners bind exclusive ephemeral IPv4 loopback ports and cannot be inherited.
The parent must own/pin private pairing/log/scratch folders and pass an inherited
stdin pipe. No PID watcher or abrupt ExitProcess is used. EOF, malformed input,
or `stop\n` cooperatively stop the daemon. Control reads are nonblocking; guest
transfers keep the existing progress deadline while checking Stop at least every
100 ms during socket retry. Main closes listeners, clients and control before
printing its retirement line. The parent must still wait/reap and drain output;
that line alone is never a clean-shutdown proof.

Readiness carries `LTW_SESSION_PROFILE` and `LTW_SESSION_TOKEN` (fresh 32-character
lowercase hex identity tags, **not secrets**) and positive canonical decimal
`LTW_SESSION_GENERATION` (at most 2^53-1). The parent binds that line to the
owned process handle's PID/FILETIME and selected saved profile. It invalidates
availability before Stop and when identity/liveness changes. It does not launch
the child, validate ACLs, enable Apps or prove Windows endpoint behavior.

## Next required work

The bounded MinGW transport build and compiler/PE closure/license inventory
are complete; the remaining mobile/TLS providers still need a build. Actual
Windows must verify private-folder pins/ACLs, Unicode
paths, launch/readiness, foreign/malformed endpoints, blocked/NAKing/silent guest
Stop, failed startup, two saved-device isolation and exact process retirement.
The held child process must supply identity directly (GetProcessTimes), never
by reopening a receipt PID. Shared native loader/allocator/private storage hooks
remain the desktop owner's scope. Local IPA/Legacy Store and persistence target
acceptance remain required after transport qualification.
