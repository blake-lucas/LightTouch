/*
 * macOS-app additions to the qemu-ios-ui.h ABI (see contrib/ios-app).
 * Same rules: app-thread safe, everything is marshaled onto the QEMU
 * thread through a bottom half.
 */

#ifndef QEMU_MACOS_EXTRAS_H
#define QEMU_MACOS_EXTRAS_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Second finger for pinch/rotate, on QEMU's multi-touch path (slot 1,
 * distinct tracking id). Finger 0 stays on the legacy abs path in
 * qemu_ios_ui_touch(); mixing the two is what the panel model expects
 * from the existing tooling. Phases are QEMU_IOS_TOUCH_*.
 */
void qemu_ios_ui_touch2(int phase, double nx, double ny);

/* Rotate the device (Meta+Left/Right chord); edge-triggered, self-releasing. */
void qemu_ios_ui_rotate(bool clockwise);

/*
 * A host key event by macOS virtual keycode (NSEvent.keyCode / kVK_*),
 * through QEMU's generated qemu_input_map_osx_to_qcode, so the app can forward
 * key events without carrying QKeyCode constants. Unmapped keycodes, Command
 * and Control are ignored.
 */
void qemu_ios_ui_key_mac(int mac_keycode, bool down);

/* Shake gesture (machine property "accel-shake"). */
void qemu_ios_ui_shake(void);

/* Degrees, right edge down / top edge away positive. pose: 0 upright, 1 flat. */
void qemu_ios_ui_attitude(double pitch_deg, double roll_deg, int pose);

/* Target capacity 0..100; charging 0 auto, 1 on, 2 off. Returns whether queued. */
bool qemu_ios_ui_battery(int level, int charging);
bool qemu_ios_ui_battery_config(int level, int charging, double drain);
bool qemu_ios_ui_usb_connection(bool attached);
/* Magnetic heading 0..359 degrees for the compass (machine "compass-heading");
 * false where the machine has none (the iPod). */
bool qemu_ios_ui_compass(int heading_deg);
/* Whether the USB host grants a high-power port's current (machine
 * "usb-charger": the iPad charges); applies at the next USB enumeration.
 * false where the machine has no such control. */
bool qemu_ios_ui_usb_charger(bool high_power);
/* The cellular modem (the machine's "baseband-modem" child: -M iPhone-2G, or
 * baseband=on on iPhone-4/n88). modem_set writes one of its properties from its
 * string form on the QEMU thread (carrier, mcc-mnc, registered, sim-present,
 * signal-dbm, incoming-call, remote-answer, remote-hangup, incoming-sms, and
 * gps-fix where the modem carries a GPS receiver, which the status's "gps" says); false
 * when there is no modem or no such property. A property's own refusal (a bad
 * number, no channel to ring on) shows as "error" in the next status.
 * modem_status: a JSON object of the modem's state as of the last refresh, which
 * this call schedules (so poll it); NULL without a modem. Free it with modem_free. */
bool qemu_ios_ui_modem_set(const char *property, const char *value);
char *qemu_ios_ui_modem_status(void);
void qemu_ios_ui_modem_free(char *status);
/* Set the accelerometer's orientation vector outright (machine
 * "accel-orientation", 1-6), rather than stepping it like
 * qemu_ios_ui_rotate: the machine itself moves it (the iPad's power-off
 * gesture), so a relative step can land on the wrong side. */
bool qemu_ios_ui_orientation(int value);
/* The vibration motor (hw/misc/ios_vibrator.c): whether it runs now. pulses (may be NULL) counts its
 * starts since the process began, so a buzz shorter than the caller's poll still shows. Always false
 * on a machine without one (the iPods, the iPad). API 2.2. */
bool qemu_ios_ui_vibrator(uint64_t *pulses);
/* 44100 Hz stereo S16LE mixer packets; read needs 16384 bytes of capacity.
 * A generation owns one recording. Empty read = 0; failed/expired = -1.
 * Empty reads with seconds >= 0 mark silence through that capture time.
 * Queued packets remain readable after stop, until the next generation. */
uint64_t qemu_ios_audio_capture_start(void);
int qemu_ios_audio_capture_read(uint64_t generation, void *buffer, int capacity, double *seconds);
double qemu_ios_audio_capture_time(uint64_t generation);
void qemu_ios_audio_capture_stop(uint64_t generation);

/* Queue UTF-8 text for the guest pasteboard (machine property "pasteboard"). */
void qemu_ios_ui_paste(const char *utf8);
/* The text last copied inside the guest (UTF-8, at most 64 KiB - 1 bytes: the guest agent's cap), when the
 * guest has copied since *serial (0 at first), which it then updates; NULL otherwise. Free it with free().
 * The agent never reports text the host gave it, nor what the guest held when the agent started. API 2.3. */
char *qemu_ios_ui_guest_pasteboard(uint64_t *serial);

/* Local bounded RPC. Request is id/op header + newline + base64 body.
 * Result is an owned id/status header + newline + base64 body, or NULL.
 * Always free a non-NULL result with qemu_ios_agent_free_result. */
bool qemu_ios_agent_request(const char *request);
/* Stops pending work; an already executed mutation cannot be undone. */
void qemu_ios_agent_cancel(const char *id);
char *qemu_ios_agent_result(void);
void qemu_ios_agent_free_result(char *result);
/* 0 absent/not running, 1 alive, 2 stale. */
int qemu_ios_agent_status(void);

/* it_boot's last QC_PKG_REPORT since the guest last reset: the serial now
 * current and its result code (contrib/it-boot/it_boot.c R_*; negative = an
 * install failed and the previous package kept running). false = no report
 * yet (no loader, or no offer this boot). */
bool qemu_ios_guest_package_report(int64_t *serial, int32_t *result);
/* The GL shim's wire protocol from QC_GLES_HELLO, and its package serial;
 * 0 (today's wire) when no hello arrived since the guest last reset. */
int32_t qemu_ios_gles_protocol(int64_t *serial);

/* Live host GL contexts cannot be included in a snapshot. */
int qemu_ios_gles_contexts(void);
/* Loaded Mach-O UUID; NULL if unavailable. */
const char *qemu_ios_build_id(void);

/* This C API's version, major << 16 | minor. A minor bump only adds entry points;
 * a major bump removes or changes one, and a host built for another major refuses
 * the dylib. */
#define QEMU_IOS_API_VERSION ((2u << 16) | 3u)
uint32_t qemu_ios_api_version(void);

/* Machine controls. */
void qemu_ios_ui_pause(void);
void qemu_ios_ui_resume(void);
void qemu_ios_ui_reset(void);
void qemu_ios_ui_powerdown(void);
void qemu_ios_ui_quit(void);
/* The guest powers itself off. 1: the guest agent was asked to halt (reboot2(RB_HALT), the call
 * SpringBoard's slide-to-power-off ends in; launchd stops every job, syncs and halts); its reply, if any,
 * carries the agent id "qemu-ios-shutdown". 2: no live agent (iPhone OS 1.x): the board's power-off
 * gesture, as qemu_ios_ui_powerdown. 0: nothing started (not ready, or NAND storage failed). Completion
 * is the guest's halt: qemu_ios_ui_guest_shutdown_confirmed(). */
int qemu_ios_ui_shutdown(void);
/* Attach or detach the emulated USB keyboard (usb-kbd on usb-bus.0); detached, iOS shows its
 * on-screen keyboard. false where the machine has no usb-bus.0 (n18, n88, the S5L8900/S5L8720 boards). */
bool qemu_ios_ui_hardware_keyboard(bool attached);

/* Flip a running user netdev's slirp restrict flag in place (id NULL/empty =
 * the only user stack). restrict=false opens outbound networking without a
 * link event, so the guest keeps its Wi-Fi association and DHCP lease. */
void qemu_ios_ui_net_restrict(const char *id, bool restrict_);
/* Allow or refuse the guest's traffic to the Mac's local networks (private, link-local,
 * multicast) on a running user netdev, or on every one for a NULL or empty id; boot with
 * -netdev user,...,lan=off to start refused. */
void qemu_ios_ui_net_lan(const char *id, bool allowed);

#ifdef __cplusplus
}
#endif

#endif /* QEMU_MACOS_EXTRAS_H */
