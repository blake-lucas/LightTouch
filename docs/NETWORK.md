# Guest networking

Android exposes **Settings → Network**. Networking and local-network access are
off by default. Enable networking and choose Direct internet or Wayback Machine;
the selected calendar date uses the Archive's closest available capture. Every
setting applies to the running session, turning networking on or off included:
changes apply to new requests immediately. Reload the page after changing them.
In-flight requests retain their original route/date. The first time networking is
on in a session, the guest is set up as at a networked start (below).
No capture means an archive error, never a live-web
fallback. Archived browsing supports GET/HEAD; forms need Direct internet.

The screen uses the app's existing Compose settings components. Dates are strict
Gregorian calendar days, from 1996 through today, serialized as `yyyyMMdd` without
timezone conversion. An atomic `files/network.json` document synchronizes the UI
and isolated engine processes. Missing or invalid preferences disable networking.
Current session presents labeled informational lines for the applied connection,
archive date, guest setup and web activity. **Ready** means the certificate and
proxy settings were installed; it does not prove a website loaded. Activity counts
connections as soon as they reach the proxy, before headers or TLS complete, and
separately reports connections in progress, finished connections, errors and the
last result. With the Wayback Machine the last result names the copy served: the
chosen day's, or (when that day has none) the nearest capture's date. Finished includes errors; totals cover this running session. Guest
setup failures and the last failed result are explicitly labeled and colored.

## Shared backend

`Portable/network` is a C++17/OpenSSL 3 library with no Android dependencies. It
implements HTTP framing, origin DNS, verified modern upstream HTTPS, the legacy
guest TLS bridge, dated archive routing, redirect handling, a bounded memory cache,
one archive fetch per second, and Retry-After cooldowns. Guest credentials and
cookies are excluded from archive requests. Direct browsing preserves cookies.
Bodies, headers, uploads, redirects and concurrent connections are bounded.
`Proxy::configure` publishes a short-locked configuration snapshot independently
of archive fetches. The bounded host cache keys captures by URL and selected date,
including responses completing after a date change. Archive responses disable
guest caching so the unchanged guest URL cannot hide a newly selected date.

Android contributes persistence, Conscrypt root certificates, JNI and worker
lifecycle. The proxy identity is per emulated device, in private app storage; only
its public certificate is sent into the guest. The local guest TLS leg permits TLS
1.0/SHA-1 for old iOS; origin TLS requires TLS 1.2 or newer with host trust and
hostname verification. Android's trust store is never changed. Certificate-pinning
apps can still reject interception.
Identity creation uses a private file lock and atomic rename, since Android app
storage can reject hard links. Interrupted or concurrent first starts preserve
one persistent CA; an existing identity is read rather than replaced.

When the guest agent is ready, typed portable operations atomically upload the
bundled signed `ittrust` and `itproxy` helpers in bounded chunks, install the public
CA and configure the Wi-Fi proxy. This supports installed packages that omit the
helpers. A lost or timed-out mutation is reported rather than replayed. Guests
without a working agent cannot complete automatic configuration.
Setup retries only requests explicitly refused before admission (busy guest tools
or a guest agent that is still starting), so other guest-tools clients can run
between uploads without aborting setup. Once accepted, a mutation is polled and
never resubmitted automatically.

The engine's bundled libslirp uses a native private Unix socket forward at
`10.0.2.100:3128`, with no subprocess or host TCP listener. The forward is added at
every boot, Off included; libslirp connects to the socket per guest connection, so
the worker makes the proxy (identity, then listener) the first time networking is
turned on, at boot or live, and a guest connection before that just closes. Off
restricts guest egress and creates no proxy identity until networking is first on.
With egress restricted (Off, and Archive) the guest's own DNS lookups get an
immediate NXDOMAIN from the virtual nameserver (engine patch
libslirp-restricted-dns.patch, zero-TTL SOA so nothing is negatively cached), so
apps fail at once instead of waiting out the resolver's timeout. Archive restricts egress to the private proxy.
Direct enables NAT and Android DNS using netd/getaddrinfo; LAN access remains an
explicit opt-in. Host loopback remains blocked. This source integration replaces
the fork PR's build-time patches.
Live NAT/LAN changes use the engine's existing app-thread-safe bottom-half hooks
through `lt_runtime_network_policy`; slirp is never accessed on the UI/Binder thread.
The worker reports its applied route/date separately from saved preferences and
retries admission during engine startup. Live changes do not reinstall guest tools.

The Rust desktop app (docs/DESKTOP.md) has the same Settings → Network. Its
per-device worker uses the same Unix listener; the host's roots go to OpenSSL as
`SSL_CERT_FILE` (the distro bundle on Linux, the system keychain's roots on macOS,
the ROOT store on Windows), since packaged builds compile OpenSSL in without
default roots. On Windows the proxy listens on an AF_UNIX socket too (Windows 10
1803 and later), and the engine's libslirp forwards to it (engine dev/desktop's
Windows `unix:` guestfwd). The other frontends do not yet expose
these settings. Upstream's retired-service and weather adapters are outside this
web-routing implementation.

## Build

Use the **dev/net** engine branch in the separate `qemu-ios-net` worktree. It starts
from engine `dev/perf` and includes slirp, Android DNS, private socket forwarding,
helper export and notice generation. Set `QEMU_IOS` to that checkout. Build from a
clean committed engine tree so its manifest and notices identify immutable input.

`scripts/build.sh android-engine` enables slirp and generates notices from the
actual selected sources/dependencies. `scripts/build.sh firmware-tools` exports
`ittrust` and `itproxy` alongside the existing helpers. Android builds require
`-PnetworkDeps` pointing to the target ABI's static OpenSSL producer prefix and
`-PguestToolsDir` containing those helpers. `scripts/build.sh android` supplies
the arm64 prefix automatically. OpenSSL notices are staged separately from engine
notices using the pinned source archive. Do not use host OpenSSL libraries for an
Android cross build. Firmware and guest assets stay outside source control.

Portable verification:

```sh
cmake -S Portable -B build/obj/network-tests \
  -DLIGHTTOUCH_QEMU_SOURCE=/path/to/qemu-ios-net \
  -DLIGHTTOUCH_BUILD_TESTS=ON -DLIGHTTOUCH_ENABLE_NETWORK=ON
cmake --build build/obj/network-tests -j6
ctest --test-dir build/obj/network-tests --output-on-failure
```

The network fixture exercises actual socket I/O, TLS 1.0 with certificate and
hostname verification, off-mode identity isolation, LAN policy, chunk decoding,
multiple response cookies, credential stripping and archive method isolation.
Activity coverage includes ingress with no HTTP bytes, concurrent connections,
one connection finishing while another waits, errors and successful responses.
Typed helper uploads also have bounds/path-injection contract tests. Android unit
tests cover default-off policy and invalid calendar dates.
Regression cases also cover admission contention between uploads, lost mutation
completions, interrupted first CA saves, and HTTP framing inside TLS tunnels.
On Linux the network fixture explicitly denies `link`/`linkat`, and verifies
first-save recovery, private key permissions and concurrent CA initialization.
The Linux live-configuration fixture uses a local verified TLS upstream and no
outbound traffic to check route/date changes, separate date caches, an old request
completing after a date change, and switching to Direct while archive I/O is blocked.
Android session tests cover startup admission retries, turning an offline boot on
live, live disable/re-enable and preserving active status on failed policy changes.

The raw NAT policy can also be checked against the engine's actual vendored
libslirp on a native host (Meson and GLib development files required):

```sh
sh Portable/tests/network/check-slirp-policy.sh /path/to/qemu-ios-net \
  /path/to/native-slirp-build [path/to/meson]
```

This runs libslirp's ping, guest-forward and NCSI suites, then checks Direct-mode
public/LAN routing, host-loopback refusal, the private Android DNS exemption and
restricted mode's immediate DNS refusal.
It opens no outbound connections.

## Android acceptance checks

For an installable non-debug APK, use the existing Android release packaging
workflow (`LT_DEBUGGABLE=false`, `LT_PROFILEABLE=false` in `scripts/build.sh
android`). Verify the APK signature and its final manifest's `debuggable=false`
before handing it off. Keep source and engine revision receipts beside the APK.

Device acceptance still requires a stopped guest with current guest tools. Check
Settings → Network is off on fresh app data, enable Direct, start the guest and
wait for **Ready** under Current session. Browse an HTTP and an HTTPS page in
Safari. While it runs, select Wayback with date **9 September 2009**, reload and
verify a page with a capture on a nearby day loads (for example,
`http://example.com/`, whose nearby capture is 10 September 2009). Also check a
missing capture gives an archive error. Return networking to off and stop the guest
after testing. Host HTTP/TLS tests do not establish Safari or guest trust-store
acceptance; those checks need the physical device.
Also change the archive date live, reload, and confirm the Current session date
and the retrieved capture change. Switch back to Direct and reload without a
restart. A nearest available capture may be the same for two nearby chosen dates.
