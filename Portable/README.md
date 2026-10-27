The portable product domain and runtime expose ABI 1 through `include/lighttouch.h`.
The runtime negotiates upstream QEMU API major 2 and portable engine extension 1.
It includes only exported engine headers; QEMU/platform internals stay out of the
public product ABI. Android (Kotlin/JNI) and the Rust desktop app consume the same native implementation.

Prepared imports validate the expected archive SHA256, strict JSON index, relative
paths, duplicates, prefix conflicts, zlib checksum and exact compressed/uncompressed
lengths. Every recorded output checksum, identity hash, NAND page count and full
`path sha256\n` NAND listing is checked before atomic promotion. Files/directories
are owner-readable and not writable. Import destination's private parent must exist.
Directory imports copy only regular files and reject symlinks. Sources are never
modified. Cancellation removes unpublished staging; maximum declared data is 64 GiB
(or a smaller caller bound). Index maximum is 64 MiB and one million entries.

A native library stores stable random record IDs, names, board/build/base integrity,
immutable copied ROM/package assets and independent base/state directories. Record
imports publish atomically; cross-process library mutations use a nonblocking flock.
Listing and selected ID survive process restart; selection is a native atomic file.
One-time legacy adoption holds the old device lease and copies retained state,
including dirty/pin/overlay, into an empty library without deleting originals. It
never replaces the baseline when another board is imported. Asset hashes are pinned
and checked when a library device opens. Host/guest capabilities remain engine facts
queried per record, not inferred from firmware metadata.

A device owns one nonblocking exclusive `flock` on its private writable state. Its
base lock checksum pins the state; a mismatched or unidentified state is refused.
Each open verifies immutable permissions and checksums again. NOR is copied into
state atomically, NAND writes go to the overlay, and `.dirty` is synced before engine
start. Only confirmed guest power-off, successful engine exit and no storage fault
clear the marker. Cold recovery is explicit and preserves all existing state; it
makes no filesystem repair promise. A failed recovery stays dirty and reports its
error. Device close is valid only after runtime disposal (callers serialize handle
lifetime operations).

Native boot assembly supports n72ap iboot/bootrom and n81ap kboot. Machine names,
display dimensions, contact limits and sensor capabilities come from engine facts.
n81ap uses prepared kboot.bin/nand-overlay/nor-rw and identity die-id; no hardware
ROM or universal RAM override is imposed. Other preparation routes return unsupported.
`IT_TVOUT_READY=1` matches the original production n72 `BootRecipe.iPod` environment.
Private .itpack archives are bounded to 64 MiB, checksum-validated against their
manifest, selected by board/build/host protocol and composed into ephemeral state
work/guest-offer with the prepared lock's hook policy. Host executables are never
launched from guest package content.

One engine initialization is allowed in each process, including failed starts.
Reboot is a fresh service/worker process. Android's service owns JNI/runtime handles;
Linux's parent execs a native worker with private inherited UID-checked Unix sockets.
Queued control requests are distinct from confirmed guest shutdown and touch batch
execution. Pause state is polled from the engine. No snapshots are advertised.
Shutdown timeout leaves dirty data; callers terminate the worker process.

Frame copies hold the engine mutex. A supervisor publishes latest BGRA frames to a
size-sealed private memfd; consumers get a readonly FD, close it after mapping and
use the even/equal generation check around copies. Slow consumers lose old frames.
The FD mapping and all handles have explicit ownership in the header. PCM capture
is the existing real QEMU 44100Hz stereo S16LE mixer capture, drained into sixteen
bounded packets; overflow drops oldest host packets and increments dropped_frames.
The desktop worker plays PCM with cpal. Silence/host queues do not prove audible guest output; real acceptance is
required. Frame/PCM transport does not use JSON or Binder frame parcels.

Touch preserves stable IDs, phased whole batches and monotonic timestamps. Native
mapping rejects letterboxes and applies quarter-turn rotation. Motion includes
gravity in m/s² and angular velocity rad/s in guest portrait axes. Android samples
use BOOTTIME and native mapping converts sample age to CLOCK_MONOTONIC using the
current source clock. Samples older than 250ms or above 200Hz are rejected. Acceleration and gyro have
independent admission clocks, including equal interleaved timestamps at exactly
200Hz; one stream cannot starve the other. Host
sensor availability is separate from engine guest capability. Held input/live motion
is cancelled on pause/focus loss; none is saved across process restarts.

Build and synthetic checks (these are not guest acceptance):

```sh
cmake -S Portable -B /private/build/portable \
  -DLIGHTTOUCH_QEMU_SOURCE=/path/to/qemu-ios -DLIGHTTOUCH_BUILD_TESTS=ON
cmake --build /private/build/portable -j2
ctest --test-dir /private/build/portable --output-on-failure
```

The shared code requires C++17 and zlib (NDK supplies zlib); hashing/JSON have no
platform dependency. The decoder is a native port of the original PreparedBase,
GuestPack and BootRecipe contracts and retains their project licensing. Do not
link GPLv3 firmware dependencies into the GPLv2-only engine process; firmware
preparation runs in a separately owned service/process.

Typed guest operations use the existing agent RPC with bounded UTF-8 arguments and
owned results: ping, frontmost, launch a builtin, type text, sync, and launch an
explicitly authorized installed gyro workload. Requests are asynchronous and at
most one is pending; cancellation/timeout never retries a possibly applied mutation.
Gyro/motion qualification is DEFERRED by explicit user scope update, not verified
and not an initial completion dependency. Existing APIs/code are preserved; no
historical SDK/probe acquisition/build or further gyro integration is initial work.
Engine sensor bits describe modeled hardware and never advertise qualified gyro. Prepared outputs.guest_package may
name a checksum-proven raw guest.itpack; library import and boot assembly select it
natively when no explicit package is supplied, so firmware UI does not choose an
architecture or recipe.
The runtime's internal PCM queue advances an epoch and discards queued samples
on pause/resume admission. The publisher owns capture stop/start and rejects an
old read's append under the same queue mutex. The publisher withholds Resume until actual PAUSED is observed, even for
a quick Pause→Resume sequence, then waits for RUNNING. Capture starts again only
after that barrier; its new backend generation clears the backend ring. PCM timestamps
remain relative to that active capture generation, so a restart may reset them;
they are not host monotonic frame timestamps. Native dropped-frame counters count
known queued/in-flight discarded frames, not unread bytes cleared inside the
backend capture ring. No public ABI signatures change.

Stop immediately closes the PCM epoch/queue under that mutex. It discards an
unsubmitted Pause, but waits for a submitted Pause to become PAUSED, submits
Resume, and waits for RUNNING before dispatching guest shutdown. The original
shutdown request clock and frontend/worker budgets are unchanged; missing VM
acknowledgement cannot extend the deadline or clear dirty state. A buffer copied
before Stop still needs the frontend's own write fence.

A buffer already returned to a frontend by `lt_runtime_pcm` cannot be revoked.
Each frontend must pause/flush its sink and fence reader/transport packets at its
own lifecycle boundary. Linux uses private command/epoch receipts; Android must
independently reject its reader's in-flight packets before AudioTrack writes.
These source invariants do not establish physical playback or target acceptance.

Post-APK23 frame storage uses the existing engine metadata probe before allocating
publisher scratch pixels. No64MiB scratch buffer is allocated at runtime creation.
Valid new metadata grows storage to the required byte count; at most three copy/
growth attempts follow each probe. Repeated growth, no frame or not ready leaves
the published frame, serial and counters unchanged. Malformed/oversize metadata
fails rather than allocating beyond the shared-frame bound. Storage is retained
for reuse; growth can transiently hold old and new allocations. The sealed shared
FD still reserves64MiB of virtual capacity, and this change does not reduce engine
frame buffers, frontend buffers, GPU allocations or prove an RSS improvement.

The additive `lt_runtime_frame_metadata` query returns published metadata only:
`LT_OK` copies no pixels, `LT_AGAIN` means absent/not newer/unstable publication.
It accepts since_sequence by value and never advances a consumer's sequence.
Fields are validated only after up to three equal/even generation snapshots;
publication and metadata reads use per-field atomic operations under the same
generation bracket without changing the physical structs/layout/version. Empty
or failed queries leave caller metadata unchanged. Invalid coherent bounds report
`LT_ENGINE`; invalid caller size/version remains an ABI error.

Published metadata may lag the direct engine `copy_frame` path. A caller must
bound its grow/copy retry count, validate the actual successful copy's shape/
stride/byte count and update its retained sequence only after accepting pixels.
If storage becomes insufficient, retain the prior sequence and retry a fresh
metadata query later; never parse an error message for a required size. No64MiB
fallback or unchanged-frame busy loop is necessary. APK23/sealed Linux32c5 use
the earlier producer; these post-freeze source changes and host regressions need
a separate build/allocation and do not qualify guest rendering or memory savings.
