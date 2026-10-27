/*
 * The ABI an iOS app uses to drive this emulator. Kept free of QEMU headers so
 * the app can include it directly, and small enough that dlsym'ing each entry
 * point by hand stays reasonable.
 */

#ifndef QEMU_IOS_UI_H
#define QEMU_IOS_UI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define QEMU_IOS_TOUCH_BEGIN  0
#define QEMU_IOS_TOUCH_UPDATE 1
#define QEMU_IOS_TOUCH_END    2

#define QEMU_IOS_BUTTON_HOME        0
#define QEMU_IOS_BUTTON_POWER       1
#define QEMU_IOS_BUTTON_VOLUME_UP   2
#define QEMU_IOS_BUTTON_VOLUME_DOWN 3

/*
 * Runs qemu_init(), the main loop and cleanup on the calling thread. Call it
 * on a background pthread; it does not return until the VM stops. QEMU cannot
 * be started twice in one process, so call it once per app launch.
 */
int qemu_ios_main(int argc, char **argv);

/*
 * What an app needs to know about a machine BEFORE it boots it: the board it
 * is, how big a window to open, which way up, which controls it has. Static per
 * machine, so it is valid before qemu_ios_main() and needs no lock. These are
 * the emulator's facts; an app keeps no copy. Unknown name -> NULL.
 */
typedef struct {
    const char *machine;        /* the -M name */
    const char *board;          /* the board ID iBoot reports ("n72ap") */
    int screen_width;           /* framebuffer pixels at default_orientation */
    int screen_height;
    int screen_scale;           /* points per pixel */
    int default_orientation;    /* 0 portrait, 1 landscape */
    bool has_cellular;          /* a modem: qemu_ios_ui_modem_* (baseband=on on iPhone-4/n88) */
    bool has_usb_host;          /* usb-bus.0: -device usb-kbd, qemu_ios_ui_hardware_keyboard */
    bool has_compass;           /* qemu_ios_ui_compass */
    bool has_usb_charger;       /* qemu_ios_ui_usb_charger */
    /* What panel=WxH accepts, as the panel scans: both sides >= panel_min, the
     * width a multiple of panel_width_step, and at most panel_max_pixels pixels
     * (0: no bound beyond the sides). */
    int panel_min;
    int panel_max_width;
    int panel_max_height;
    int panel_width_step;
    int panel_max_pixels;
} QemuIosDeviceInfo;

const QemuIosDeviceInfo *qemu_ios_device_info(const char *machine);
/* The index-th machine this library runs, for listing; NULL past the last. */
const QemuIosDeviceInfo *qemu_ios_device_info_at(int index);

/* Called on the QEMU thread whenever a new frame is ready. Do not block. */
typedef void (*qemu_ios_frame_cb)(void *opaque);

/*
 * Attach the app's display. Safe to call before the VM has a console; the
 * listener is registered as soon as one exists.
 */
void qemu_ios_ui_attach(qemu_ios_frame_cb cb, void *opaque);

/* Called by qemu_ios_main() itself once the VM exists; not for app use. */
void qemu_ios_ui_vm_started(void);
/* Internal diagnostic lifecycle; called only by the embedded entry point. */
void qemu_ios_frame_path_init(void);
void qemu_ios_frame_path_finish(void);

/*
 * True only while the emulator is initialized and its main loop is running.
 * Entry points that schedule bottom halves must check this: before qemu_init()
 * the AioContext is NULL, and after the main loop returns nothing services it.
 */
bool qemu_ios_ui_ready(void);
/* Optional bounded profiling capture. Thread safe; paths are host paths.
 * status: 1 recording, 0 idle, negative errno on write/limit failure.
 * LT_PROFILE=off disables capture. No guest instruction instrumentation. */
int qemu_ios_profile_start(const char *path);
int qemu_ios_profile_stop(void);
int qemu_ios_profile_status(void);
/* Engine instrumentation only; begin/end use QEMU_CLOCK_REALTIME ns. */
uint64_t qemu_ios_profile_begin(void);
void qemu_ios_profile_gl(uint64_t start, uint32_t slot, uint32_t context);
void qemu_ios_profile_lock(uint64_t start, uint64_t end, bool wait,
                           const char *file, int line);
void qemu_ios_profile_callback(void *fn, const char *name, int64_t ns);
/* Session-latched host NAND I/O failure; thread-safe to poll from the UI. */
bool qemu_ios_ui_storage_failed(void);

/* True only after the guest writes the final PMU power-off command. */
bool qemu_ios_ui_guest_shutdown_confirmed(void);
/* Guest backlight state, independent of black framebuffer contents. */
bool qemu_ios_ui_display_sleeping(void);
/* The level the guest last programmed into the backlight driver, as the raw code that driver takes (the
 * 2G's D1759 WLED 0x30, the 1G's PCF50633 LEDOUT 0x28, the A4's SWI level); 0 with the light off, -1 where
 * the board's backlight is not decoded. API 2.1. */
int qemu_ios_ui_backlight_level(void);
void qemu_ios_ui_vm_stopped(void);

/*
 * The newest frame as tightly packed BGRA, WITHOUT a copy. `serial` is in-out:
 * pass the last one you saw and this returns false if nothing is newer, which
 * is the common case (the guest paints slower than the display refreshes).
 * The pixels stay valid for a few more frames; never free them.
 */
bool qemu_ios_ui_frame(const void **pixels, int *width, int *height,
                       uint64_t *serial);

void qemu_ios_ui_frame_size(int *width, int *height);

/*
 * Touch position is normalized 0..1 over the guest screen, y downwards.
 * One finger only for now -- see the note in qemu-ios-ui.c.
 */
void qemu_ios_ui_touch(int slot, int phase, double nx, double ny);

/*
 * A hardware button, pressed and released as separate calls. Home and power
 * are the two that matter: an emulated device that has gone to sleep cannot be
 * woken any other way.
 */
void qemu_ios_ui_button(int button, bool down);

/* Generic button/single-touch sequences in virtual milliseconds, not wall
 * time. Arrays have count elements; transitions must be balanced and ordered.
 * A new submission cancels/releases the prior sequence. Manual input cancels
 * it too; held manual input causes admission refusal. The immediate return
 * means queued, not executed; poll the most recent ID for actual admission.
 * Explicit cancellation releases only owned signals even while paused.
 * This is host automation state and is not part of a guest snapshot. */
#define QEMU_IOS_INPUT_UNKNOWN 0
#define QEMU_IOS_INPUT_RUNNING 1
#define QEMU_IOS_INPUT_DONE 2
#define QEMU_IOS_INPUT_CANCELLED 3
#define QEMU_IOS_INPUT_REJECTED 4
bool qemu_ios_ui_input_sequence(uint64_t id, size_t count,
    const int64_t *at_ms, const int32_t *kind, const int32_t *value,
    const int32_t *phase, const double *x, const double *y);
int qemu_ios_ui_input_sequence_status(uint64_t id);
void qemu_ios_ui_input_sequence_cancel(uint64_t id);
/* Internal: QEMU thread under BQL, before reset/quit/semantic powerdown. */
void qemu_ios_ui_cancel_input(void);
void qemu_ios_ui_manual_touch2(bool down);
/* Internal: run fn(opaque) on the QEMU thread, after every host input queued before it (touches, buttons, keys). */
void qemu_ios_ui_input_later(void (*fn)(void *), void *opaque);

/*
 * Whether the app is foreground. iOS kills a process that issues GL commands
 * while it is not, so this must be cleared BEFORE the app is backgrounded --
 * on willResignActive, not on didEnterBackground.
 */
void qemu_ios_set_foreground(bool foreground);

/*
 * Save the machine to `path`, so the next launch can restore instead of
 * booting (`-incoming file:<path>`). Asynchronous: _status() distinguishes "not
 * started", "in flight", "done" and "failed", and fills errbuf with the reason
 * on failure. _save2 sets the status to RUNNING on the calling thread before
 * scheduling the work, so a status() call that races ahead of the bottom half
 * never reads a stale DONE. The guest is stopped as a side effect.
 */
typedef enum {
    QEMU_IOS_SNAPSHOT_IDLE = 0,
    QEMU_IOS_SNAPSHOT_RUNNING = 1,
    QEMU_IOS_SNAPSHOT_DONE = 2,
    QEMU_IOS_SNAPSHOT_FAILED = 3,
} QemuIosSnapshotStatus;

void qemu_ios_snapshot_save2(const char *path);
QemuIosSnapshotStatus qemu_ios_snapshot_status(char *errbuf, unsigned long errlen);

/* Resume the vCPU after a completed save, without touching foreground state
 * (qemu_ios_set_foreground(true) is the only other thing that restarts it). */
void qemu_ios_snapshot_resume(void);

/* Portable extension 1.0. Existing qemu_ios API 2.2 remains unchanged.
 * Caller owns all structs/buffers. Calls may originate on the host thread;
 * queued input executes on the engine thread. One engine per process. */
#define QEMU_IOS_PORTABLE_VERSION 1u
#define QEMU_IOS_PORTABLE_MAX_CONTACTS 8u
#define QEMU_IOS_CAP_ACCEL (1ull << 0)
#define QEMU_IOS_CAP_GYRO (1ull << 1)
#define QEMU_IOS_CAP_MULTITOUCH (1ull << 2)
#define QEMU_IOS_CAP_FRAME_COPY (1ull << 3)
#define QEMU_IOS_CAP_HEADPHONES (1ull << 4)
#define QEMU_IOS_TOUCH_CANCEL 3
#define QEMU_IOS_MOTION_ACCEL 1u
#define QEMU_IOS_MOTION_GYRO 2u

typedef enum {
    QEMU_IOS_OK = 0, QEMU_IOS_QUEUED = 1, QEMU_IOS_NO_FRAME = 2,
    QEMU_IOS_INVALID = -1, QEMU_IOS_NOT_READY = -2,
    QEMU_IOS_UNSUPPORTED = -3, QEMU_IOS_BUFFER_SMALL = -4,
    QEMU_IOS_BUSY = -5, QEMU_IOS_STALE = -6,
} QemuIosResult;

typedef struct {
    uint32_t size, version;
    char machine[32], board[16];
    uint32_t width, height, scale, orientation, max_contacts;
    uint64_t capabilities;
} QemuIosPortableBoard;

typedef struct {
    uint32_t size, version;
    uint32_t width, height, stride, format; /* format 1: tightly packed BGRA8 */
    uint64_t serial, timestamp_ns, required_bytes;
} QemuIosPortableFrame;

typedef struct {
    uint32_t size, version;
    uint32_t contact_id, phase;
    double x, y; /* normalized panel coordinates, x right / y down */
} QemuIosPortableContact;

typedef struct {
    uint32_t size, version;
    uint64_t timestamp_ns; /* monotonic host clock, strictly increasing per field */
    uint32_t fields, reserved;
    /* Guest portrait axes: x right, y up, z toward viewer.
     * Acceleration includes gravity in m/s^2; angular velocity in rad/s.
     * Samples expire after 250 ms virtual time; no host input is migrated. */
    double acceleration[3], angular_velocity[3];
} QemuIosPortableMotion;

uint32_t qemu_ios_portable_version(void);
int qemu_ios_portable_board(const char *machine, QemuIosPortableBoard *out);
int qemu_ios_portable_board_at(int index, QemuIosPortableBoard *out);
bool qemu_ios_ui_paused(void);
/* Copy holds the frame mutex until memcpy completes. On BUFFER_SMALL,
 * metadata describes the required buffer; serial is not acknowledged.
 * last_serial is caller's previous frame. timestamp_ns is publication time:
 * host CLOCK_MONOTONIC nanoseconds on Linux/Bionic, continuing while paused.
 * It is not a wall-clock epoch or VM virtual time. */
int qemu_ios_portable_frame_copy(void *pixels, size_t capacity,
    uint64_t last_serial, QemuIosPortableFrame *out);
/* Entire batch validates before injection; stable IDs survive slot reuse.
 * Return QUEUED means accepted into bounded engine queue, not delivered.
 * status(id) returns QUEUED/OK or rejection; poll newest submission id only.
 * Each batch closes with exactly one sync, DATA before BEGIN/UPDATE. */
int qemu_ios_portable_touch(uint64_t id, uint64_t timestamp_ns,
    const QemuIosPortableContact *contacts, size_t count);
int qemu_ios_portable_input_status(uint64_t id);
/* Release all owned touch/buttons and clear live motion, including paused VM. */
int qemu_ios_portable_cancel_input(void);
int qemu_ios_portable_motion(const QemuIosPortableMotion *sample);

#ifdef __cplusplus
}
#endif

#endif /* QEMU_IOS_UI_H */
