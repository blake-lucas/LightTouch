#ifndef LIGHTTOUCH_H
#define LIGHTTOUCH_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
#define LT_ABI_VERSION 1u
#define LT_MAX_CONTACTS 8u
#define LT_FRAME_CAPACITY (4096u * 4096u * 4u)
/* Host deadlines begin after requesting semantic guest shutdown, separately
 * from import/startup. Callers may extend observation; never shorten to infer a
 * clean exit. Confirmed cleanup may finish sooner. Kill/timeout retains dirty. */
#define LT_SHUTDOWN_CONFIRMATION_TIMEOUT_MS 30000u
#define LT_SHUTDOWN_GRACE_MS 35000u
/* A Stop before a booting guest's agent first answers (on a device whose agent
 * answered on an earlier boot) waits up to this long for it before asking iOS;
 * callers count their deadlines from when iOS is asked (shutdown_route set). */
#define LT_SHUTDOWN_AGENT_WAIT_MS 20000u
#define LT_INIT(type) {sizeof(type), LT_ABI_VERSION}
typedef enum {
    LT_OK = 0,
    LT_INVALID = 1,
    LT_IO = 2,
    LT_BUSY = 3,
    LT_UNSUPPORTED = 4,
    LT_ENGINE = 5,
    LT_DIRTY = 6,
    LT_CANCELLED = 7,
    LT_AGAIN = 8
} lt_status;
typedef struct {
    uint32_t size, version;
    int32_t code;
    char message[512];
} lt_error;
typedef struct lt_device lt_device;
typedef struct lt_runtime lt_runtime;
typedef struct lt_library lt_library;
/* Durable private device library. Records retain independent immutable bases,
 * writable state and asset copies. Selection is native, atomic and persistent.
 * All callers serialize handle lifetime; cross-process mutations use flock. */
typedef struct {
    uint32_t size, version;
    const char *root_path;
} lt_library_config;
typedef struct {
    uint32_t size, version;
    char id[64], display_name[128], board[32], build[32], base_sha256[65];
    char base_path[1024], state_path[1024], bootrom_path[1024], guest_package_path[1024];
    uint32_t selected, dirty;
} lt_record_info;
lt_status lt_library_open(const lt_library_config *, lt_library **, lt_error *);
void lt_library_close(lt_library *);
lt_status lt_library_count(lt_library *, uint32_t *, lt_error *);
lt_status lt_library_record_at(lt_library *, uint32_t index, lt_record_info *, lt_error *);
lt_status lt_library_select(lt_library *, const char *record_id, lt_error *);
lt_status lt_library_selected(lt_library *, lt_record_info *, lt_error *);
lt_status lt_library_device_open(lt_library *, const char *record_id, uint32_t allow_dirty, lt_device **, lt_error *);
/* Rename a record (1..127 bytes of UTF-8). Allowed while the device runs. */
lt_status lt_library_rename(lt_library *, const char *record_id, const char *display_name, lt_error *);
/* Permanently delete a stopped record: base, writable state and assets. LT_BUSY while its device
 * is open. If it was selected, the first remaining record (if any) becomes selected. */
lt_status lt_library_remove(lt_library *, const char *record_id, lt_error *);

/* All paths UTF-8, absolute, private app-owned directories. Destination must not
 * exist. Imports stage beside destination and atomically publish after validation.
 * expected_sha256 is mandatory for archives; folder import verifies lock outputs.
 * Progress callback may cancel by returning nonzero, called on importing thread. */
typedef int (*lt_progress_fn)(void *, uint64_t, uint64_t);
typedef struct {
    uint32_t size, version;
    const char *source, *destination, *expected_sha256;
    uint64_t max_bytes;
    lt_progress_fn progress;
    void *opaque;
} lt_import_config;
typedef struct {
    uint32_t size, version;
    const char *source_path, *expected_sha256, *display_name, *bootrom_path, *guest_package_path;
    uint64_t max_bytes;
    lt_progress_fn progress;
    void *opaque;
} lt_library_import_config;
lt_status lt_library_import(lt_library *, const lt_library_import_config *, lt_record_info *, lt_error *);
/* Replace a stopped record's guest package (assets/guest.itpack and its pinned SHA-256), keeping
 * its base and state: the guest's loader installs the new package over the existing system at the
 * next boot. LT_BUSY while the record's device is open. path must be a regular ITPACK01 file. */
lt_status lt_library_set_guest_package(lt_library *, const char *record_id, const char *path, lt_record_info *,
                                       lt_error *);
/* Transactionally copy a stopped legacy base/state into a new record while
 * holding its writer lease. Preserves old data, including dirty/pin/overlay.
 * Library must be empty to avoid accidentally adopting legacy data twice. */
lt_status lt_library_adopt_legacy(lt_library *, const lt_library_import_config *, const char *legacy_state_path,
                                  lt_record_info *, lt_error *);
lt_status lt_import_archive(const lt_import_config *, lt_error *);
lt_status lt_import_directory(const lt_import_config *, lt_error *);
/* Additive phase observer; existing config/callback ABI remains unchanged.
 * phase is hashing, extracting, or verifying; done/total refer to that phase.
 * A nonzero return cancels before publication and removes staging. */
typedef int (*lt_import_phase_fn)(void *, const char *phase, uint64_t done, uint64_t total);
lt_status lt_import_archive_observed(const lt_import_config *, lt_import_phase_fn, void *, lt_error *);
lt_status lt_import_directory_observed(const lt_import_config *, lt_import_phase_fn, void *, lt_error *);
/* lt_library_import, reporting each phase ("extracting", "verifying", ...) with its bytes done and total. */
lt_status lt_library_import_observed(lt_library *, const lt_library_import_config *, lt_import_phase_fn, void *,
                                     lt_record_info *, lt_error *);
/* lt_library_import_observed for a preparer's output folder (device.lock.json with every file's digest, as
 * firmwarekit create writes it) on the library's own storage: the folder is moved in as the record's base, not
 * copied, and admitted against its lock's digests without being read again; the new record's state keeps them, so
 * its first start reads nothing either. The folder is consumed, or handed back where it was on failure. */
lt_status lt_library_adopt_prepared(lt_library *, const lt_library_import_config *, lt_import_phase_fn, void *,
                                    lt_record_info *, lt_error *);
/* Validate/freeze and atomically publish an already-written private sibling
 * stage without copying it. Stage must be an owned 0700 directory; descendants
 * must be owned regular single-link files/directories with no external writers.
 * Failure/cancellation before publication retains the stage for caller cleanup;
 * permissions may already be frozen. Publication consumes the stage path. A
 * subsequent parent-sync error can return failure with destination published.
 * Concurrent mutation of stage during the call is forbidden. */
typedef struct {
    uint32_t size, version;
    const char *stage, *destination;
} lt_finalize_import_config;
lt_status lt_finalize_import(const lt_finalize_import_config *, lt_error *);
lt_status lt_finalize_import_observed(const lt_finalize_import_config *, lt_import_phase_fn, void *, lt_error *);
lt_status lt_sha256_file(const char *path, char hex[65], lt_error *);
/* Device handle holds an exclusive flock lease until close. Base must be separate
 * from state. State is pinned to base lock checksum. Dirty state is never erased:
 * allow_dirty permits cold recovery with existing overlay, otherwise LT_DIRTY. */
typedef struct {
    uint32_t size, version;
    const char *base_path, *state_path;
    uint32_t allow_dirty;
} lt_device_config;
typedef struct {
    uint32_t size, version;
    char board[32], build[32], boot_strategy[32], base_sha256[65];
    uint32_t dirty;
} lt_device_info;
lt_status lt_device_open(const lt_device_config *, lt_device **, lt_error *);
/* Optional synchronous admission observer. Bytes count original asset digest
 * input, with monotonic done/total; (0,0) may report initial enumeration.
 * Nonzero callback return cancels between hash chunks/stages with LT_CANCELLED,
 * null output and released lease. Do not reenter device/library APIs from the
 * callback. A null observer preserves ordinary opening and every integrity,
 * mode/path/identity/dirty check; progress completion is not admission success. */
lt_status lt_device_open_observed(const lt_device_config *, lt_progress_fn, void *opaque,
                                  lt_device **, lt_error *);
lt_status lt_library_device_open_observed(lt_library *, const char *record_id, uint32_t allow_dirty,
                                          lt_progress_fn, void *opaque, lt_device **, lt_error *);
/* Engine-owned hardware facts. Call before boot; machine is selected by board
 * from engine discovery when boot.machine is null. Cap bits: 1 accel,2 guest
 * gyro MODEL,4 multi-touch,8 frame copy. Frontends feed host sensors only for
 * the bits a board sets. Host sensor presence is reported separately. */
typedef struct {
    uint32_t size, version;
    char machine[32], board[16];
    uint32_t width, height, scale, orientation, max_contacts;
    uint64_t capabilities;
} lt_board_info;
lt_status lt_engine_board(const char *engine_library, const char *board, lt_board_info *, lt_error *);
lt_status lt_device_describe(lt_device *, lt_device_info *, lt_error *);
void lt_device_close(lt_device *);
/* Engine facts select the machine; product never invents a machine name. ROM and
 * package remain caller supplied private assets. Shared domain assembles argv. */
typedef struct {
    uint32_t size, version;
    const char *machine, *bootrom_path, *guest_package_path;
    const char *serial_path;
    uint32_t audio_enabled;
    /* Headphones in the jack: set only where the board reports LT_BOARD_HEADPHONES. */
    uint32_t headphones;
    /* App-owned network policy. Off (zero) blocks guest egress. Direct permits
     * NAT; archive permits only the dated HTTP proxy. Proxy path is a private
     * Unix socket, not a shell command; given with Off too, the guest can be
     * switched on live once the app listens there. No frontend-supplied QEMU
     * arguments. */
    uint32_t network_mode; /* 0 off, 1 direct, 2 archive */
    uint32_t network_local;
    const char *network_socket;
} lt_boot_config;
/* lt_board_info.capabilities: the engine models this board's headphone jack. */
#define LT_BOARD_HEADPHONES (1ull << 4)
lt_status lt_device_boot_argv(lt_device *, const lt_boot_config *, int *argc, const char *const **argv, lt_error *);
/* Owned argv remains valid until device close or next boot_argv call. */
typedef enum {
    LT_STOPPED = 0,
    LT_STARTING = 1,
    LT_RUNNING = 2,
    LT_PAUSED = 3,
    LT_STOPPING = 4,
    LT_FAILED = 5
} lt_state;
typedef struct {
    uint32_t size, version;
    uint32_t state, guest_shutdown_confirmed, storage_failed, dirty;
    int32_t engine_exit;
    uint64_t frames;
} lt_runtime_status;
typedef struct {
    uint32_t size, version;
    const char *engine_library;
    lt_device *device;
    lt_boot_config boot;
} lt_runtime_config;
/* One start per OS process; create negotiates engine ABI before any start.
 * Engine runs on owned background thread. A stopped runtime cannot restart:
 * launch a fresh service/worker process. device outlives runtime. */
lt_status lt_runtime_create(const lt_runtime_config *, lt_runtime **, lt_error *);
lt_status lt_runtime_start(lt_runtime *, lt_error *);
/* Explicit diagnostic opt-in before start; compiled OFF unless
 * LIGHTTOUCH_ENABLE_DEBUG_QMP is enabled. No caller argv/path/command accepted.
 * Socket is derived private state/work/qmp-PID.sock. Long socket paths require
 * a client directory-FD alias (backend fixed readonly snapshot client handles
 * this). Does not expose arbitrary monitor commands through this product ABI. */
lt_status lt_runtime_debug_qmp_enable(lt_runtime *, lt_error *);
lt_status lt_runtime_debug_qmp_path(lt_runtime *, char *path, uint32_t capacity, lt_error *);
/* Optional pre-start wire diagnosis behind the same debug build gate.
 * Enables only ipod_touch_multitouch_log; accepts no caller argv or command.
 * Guest virtual-time wire reads are distinct from host/model acknowledgments. */
lt_status lt_runtime_debug_touch_trace_enable(lt_runtime *, lt_error *);
lt_status lt_runtime_poll(lt_runtime *, lt_runtime_status *, lt_error *);
/* Display power is separate from black pixels. Shutdown phase: 0 idle,
 * 1 host queued,2 cancelling inputs,3 engine request,4 admitted,5 PMU confirmed.
 * route: 0 none,1 agent halt,2 engine board power-off gesture. A guest reply or
 * dark screen never alone means clean shutdown. Values are observer snapshots. */
typedef struct {
    uint32_t size, version;
    uint32_t engine_ready, display_sleeping;
    int32_t backlight_level;
    /* shutdown_phase: 0 idle, 1 requested, 2 cancel, 3 submit, 4 await halt,
     * 5 confirmed, 6 await agent refresh (inside the original grace period). */
    uint32_t agent_status, shutdown_phase, shutdown_route;
    int32_t shutdown_reply_status;
    uint32_t shutdown_reply_received;
} lt_runtime_diagnostics;
lt_status lt_runtime_diagnose(lt_runtime *, lt_runtime_diagnostics *, lt_error *);
/* Producer/pull observations, not guest receipt or audible playback evidence.
 * frame_timestamp_ns uses host CLOCK_MONOTONIC on Linux/Android, as does
 * frame_published_monotonic_ns. PCM timestamps below use a separate capture
 * generation-relative epoch and cannot be subtracted from these timestamps.
 * input_requests counts attempted batches, not delivered contacts. */
typedef struct {
    uint32_t size, version;
    uint64_t frame_count, frame_sequence, frame_timestamp_ns, frame_published_monotonic_ns;
    uint64_t pcm_captured_frames, pcm_pulled_frames, pcm_dropped_frames, input_requests;
    uint32_t pcm_queued_packets, reserved;
} lt_runtime_io_stats;
lt_status lt_runtime_io_statistics(lt_runtime *, lt_runtime_io_stats *, lt_error *);

/* Optional engine recording; one active host-path capture per worker. Empty
 * path stops and flushes it. Status is 1 active, 0 idle, negative host errno. */
lt_status lt_runtime_profile(lt_runtime *, const char *path, lt_error *);
lt_status lt_runtime_profile_status(lt_runtime *, int32_t *status, lt_error *);
lt_status lt_runtime_pause(lt_runtime *, uint32_t paused, lt_error *);
/* Queue a live NAT/LAN policy on the engine loop: 0 off, 1 direct, 2 archive.
 * Requires an enabled proxy endpoint at boot; LT_AGAIN before engine readiness.
 * The frontend also updates its shared Proxy configuration. No guest tools or
 * Wi-Fi reconfiguration is needed. Callers serialize this with proxy changes. */
lt_status lt_runtime_network_policy(lt_runtime *, uint32_t mode, uint32_t local_network, lt_error *);
/* Stop requests semantic guest shutdown; poll confirmation. Timeout/kill leaves
 * dirty marker. destroy only after terminal state; returns LT_BUSY otherwise. */
lt_status lt_runtime_request_shutdown(lt_runtime *, lt_error *);
lt_status lt_runtime_destroy(lt_runtime *, lt_error *);
/* Latest frame wins. copy_frame copies under engine mutex to caller storage.
 * BGRA8888, top-left, y down, stride >= width*4. No borrowed pointer crosses ABI.
 * since_sequence in/out; LT_AGAIN when unchanged. Any caller thread, serialized. */
typedef struct {
    uint32_t size, version;
    uint32_t width, height, stride, format;
    uint64_t sequence, timestamp_ns, bytes;
} lt_frame_info;
lt_status lt_runtime_copy_frame(lt_runtime *, void *pixels, uint64_t capacity, uint64_t *since_sequence,
                                lt_frame_info *, lt_error *);
/* Bounded coherent query of published shared metadata; copies NO pixels.
 * LT_OK describes a frame newer than since_sequence; LT_AGAIN means no newer
 * coherent publication is available. since_sequence is never advanced.
 * Publication may lag the engine: required storage can change before copy_frame.
 * Callers must bound growth/retries and validate the actual successful copy.
 * This additive query preserves the existing layouts and copy_frame contract. */
lt_status lt_runtime_frame_metadata(lt_runtime *, uint64_t since_sequence, lt_frame_info *, lt_error *);
/* Wakeup for presenters that would otherwise poll frame_metadata. listener runs
 * on the runtime's publisher thread right after each publication, with the new
 * sequence; it must not block or call into the runtime. NULL clears it; once
 * this returns, no call to the previous listener is in flight. Clear it before
 * lt_runtime_destroy. Additive; existing layouts are unchanged. */
typedef void (*lt_frame_listener)(void *opaque, uint64_t sequence);
lt_status lt_runtime_set_frame_listener(lt_runtime *, lt_frame_listener, void *opaque, lt_error *);
/* Export a private bounded shared-frame FD. Caller owns fd and closes it.
 * mmap read-only; header followed by capacity bytes; generation seqlock must be
 * equal/even before and after copying. Producer never waits for consumer. */
typedef struct {
    uint32_t size, version;
    uint64_t generation;
    lt_frame_info frame;
    uint64_t capacity;
} lt_shared_frame;
lt_status lt_runtime_frame_fd(lt_runtime *, int *fd, uint64_t *mapping_bytes, lt_error *);
/* Screenshot only (no emulator snapshot): atomically write actual copied BGRA
 * pixels as a top-down BMP. Pure encoder works for either frontend; runtime
 * convenience copies under engine frame mutex and fails if no frame exists. */
lt_status lt_frame_write_bmp(const lt_frame_info *, const void *pixels, uint64_t capacity, const char *private_path,
                             lt_error *);
lt_status lt_runtime_write_screenshot(lt_runtime *, const char *private_path, lt_frame_info *, lt_error *);

/* Stable IDs, whole batch admission, normalized screen coords y down. One
 * CLOCK_MONOTONIC timestamp per batch. Phases 0 down,1 move,2 up,3 cancel. */
typedef struct {
    uint32_t size, version;
    uint32_t id, phase;
    double x, y;
} lt_contact;
typedef struct {
    uint32_t size, version;
    uint64_t timestamp_ns;
    uint32_t count, reserved;
    lt_contact contacts[LT_MAX_CONTACTS];
} lt_touch_batch;
lt_status lt_runtime_touch(lt_runtime *, const lt_touch_batch *, lt_error *);
/* Touch call returns queued admission only. Poll newest batch result: state
 * 0 unknown,1 queued,2 delivered,3 rejected. No guest receipt claim from queue. */
typedef struct {
    uint32_t size, version;
    uint64_t request_id;
    uint32_t state;
    int32_t engine_result;
} lt_input_status;
lt_status lt_runtime_input_status(lt_runtime *, lt_input_status *, lt_error *);
lt_status lt_runtime_cancel_input(lt_runtime *, lt_error *);
lt_status lt_runtime_button(lt_runtime *, uint32_t button, uint32_t down, lt_error *);
/* Guest portrait axes: x right, y up, z toward viewer. Acceleration m/s^2,
 * angular velocity rad/s. Kind bits 1 acceleration,2 gyro. Samples are live,
 * timestamps CLOCK_MONOTONIC (Android adapter converts BOOTTIME using current
 * clock offset), max 200 Hz; discarded while paused/stale (>250ms), never persisted. */
typedef struct {
    uint32_t size, version;
    uint64_t timestamp_ns;
    uint32_t kinds, reserved;
    double acceleration[3], angular_velocity[3];
} lt_motion;
lt_status lt_runtime_motion(lt_runtime *, const lt_motion *, lt_error *);
/* Display mapping excludes letterboxes; rotation 0/90/180/270 clockwise. */
/* Sensor frame conversion and clock-domain conversion live in native domain.
 * source_timestamp_ns and source_now_ns use the same adapter clock; monotonic
 * now is sampled here, preserving age across Android suspend offsets. */
lt_status lt_map_motion(uint32_t rotation, uint64_t source_timestamp_ns, uint64_t source_now_ns, const lt_motion *raw,
                        lt_motion *portrait, lt_error *);
lt_status lt_map_contact(double px, double py, double left, double top, double width, double height, uint32_t rotation,
                         double *x, double *y);
/* Bounded PCM pull. Interleaved signed little-endian 16-bit; caller owns buffer.
 * frames counts sample frames (channels samples each). timestamp_ns is relative
 * to the active capture generation, not the frame/input host monotonic epoch.
 * LT_UNSUPPORTED until engine advertises PCM callback; never synthesizes audio. */
typedef struct {
    uint32_t size, version;
    uint32_t sample_rate, channels, frames, reserved;
    uint64_t timestamp_ns, dropped_frames;
} lt_pcm_info;
lt_status lt_runtime_pcm(lt_runtime *, int16_t *, uint32_t capacity_frames, lt_pcm_info *, lt_error *);
/* Small typed guest operations use the existing agent, never a host/guest shell.
 * Requests are async, at most one pending; caller polls completion. Lost/timeout
 * mutations are not retried. Launch builtin IDs are limited to com.apple.*;
 * A preserved gyro probe API requires an explicitly authorized already-installed workload and
 * actual engine guest gyro capability. No probe binary is synthesized. */
typedef enum {
    LT_GUEST_PING = 0,
    LT_GUEST_FRONTMOST = 1,
    LT_GUEST_LAUNCH_BUILTIN = 2,
    LT_GUEST_TYPE_TEXT = 3,
    LT_GUEST_SYNC = 4,
    LT_GUEST_LAUNCH_GYRO_PROBE = 5,
    LT_GUEST_NETWORK_CERTIFICATE = 6,
    LT_GUEST_NETWORK_TRUST = 7,
    LT_GUEST_NETWORK_ROUTE = 8,
    /* argument: decimal offset, final 0/1, base64 bytes (at most 2048).
     * Only the fixed bundled network helper destinations are permitted. */
    LT_GUEST_NETWORK_TRUST_CHUNK = 9,
    LT_GUEST_NETWORK_ROUTE_CHUNK = 10
} lt_guest_operation;
typedef struct {
    uint32_t size, version;
    uint32_t operation, timeout_ms;
    const char *argument;
} lt_guest_request;
typedef struct {
    uint32_t size, version;
    uint64_t request_id;
    uint32_t state;
    int32_t guest_status;
    uint32_t bytes, reserved;
    char output[4096];
} lt_guest_result;
/* Guest result states: 0 unknown,1 queued,2 complete,3 rejected,4 cancelled,
 * 5 timed out (outcome unknown). output bytes UTF-8; caller owns buffer. */
lt_status lt_runtime_guest_status(lt_runtime *, uint32_t *status, lt_error *);
lt_status lt_runtime_guest_request(lt_runtime *, const lt_guest_request *, uint64_t *request_id, lt_error *);
lt_status lt_runtime_guest_poll(lt_runtime *, uint64_t request_id, lt_guest_result *, lt_error *);
lt_status lt_runtime_guest_cancel(lt_runtime *, uint64_t request_id, lt_error *);
uint32_t lt_abi_version(void);
#ifdef __cplusplus
}
#endif
#endif
