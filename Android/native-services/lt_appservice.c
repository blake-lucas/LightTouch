/*
 * lt_appservice: LightTouch Android guest app-services helper.
 *
 * A short-lived, private child of the app. It talks ONLY to the app's own
 * usbmuxd-qemu daemon (--socket, an owner-only Unix socket) and never to a
 * system usbmuxd or a physical device. As the desktop app's
 * crates/lighttouch-services:
 *
 *   liblt_appservice.so info    --socket <mux.sock>
 *   liblt_appservice.so install --socket <mux.sock> --ipa <path> --bundle-id <id>
 *   liblt_appservice.so list    --socket <mux.sock>
 *   liblt_appservice.so syslog  --socket <mux.sock> --seconds <1..600>
 *
 * Output is JSON Lines on stdout (one object per line, flushed):
 *   {"target":{"product_type":"iPod2,1","ios":"3.1.3"}}       always first
 *   {"phase":"upload","percent":N}                              install
 *   {"phase":"mutation","status":"..."}                         just before instproxy_install
 *   {"phase":"install","status":"...","percent":N|null}         install callbacks
 *   {"result":{...}}                                            success, exit 0
 *   {"error":"..."}                                             failure, exit 1 (2: install watchdog)
 *
 * The IPA must already be prepared (architecture/permission checks happen in
 * the app, as Desktop's ipa.prepare does before Mobile.install).
 *
 * Pair records and the SystemBUID live in the daemon (usbmuxd -C); with
 * libimobiledevice 1.4.0 every userpref read/write goes through usbmuxd, so
 * this helper needs no configuration directory.
 *
 * Optional environment: LT_PARENT_DEATH_SIGNAL=1 (+ LT_PARENT_PID) makes the
 * helper exit if the app process dies, matching the daemon's behaviour.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <libgen.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include <libimobiledevice/libimobiledevice.h>
#include <libimobiledevice/lockdown.h>
#include <libimobiledevice/afc.h>
#include <libimobiledevice/installation_proxy.h>
#include <libimobiledevice/syslog_relay.h>
#include <plist/plist.h>

#define CLIENT_LABEL "LightTouchAndroid"
#define CONNECT_WINDOW_MS 20000
#define CONNECT_RETRY_MS 500
#define UPLOAD_CHUNK (1u << 20)         /* one AFC write each, answered before the next: see mobile.py */
#define INSTALL_ABSOLUTE_S 600       /* plus the unpacked size at UNPACK_FLOOR_BPS */
#define INSTALL_IDLE_S 90           /* no status and no unpacking progress for this long */
#define UNPACK_FLOOR_BPS (256u << 10)
#define SPACE_POLL_MS 2000
#define STAGING_DIR "PublicStaging"

/* ---------- JSON output ---------- */

struct buf {
	char *data;
	size_t len, cap;
};

static void buf_reserve(struct buf *b, size_t extra)
{
	if (b->len + extra + 1 <= b->cap)
		return;
	size_t cap = b->cap ? b->cap : 256;
	while (cap < b->len + extra + 1)
		cap *= 2;
	char *data = realloc(b->data, cap);
	if (!data) {
		/* Out of memory while reporting: nothing better to do. */
		static const char oom[] = "{\"error\":\"out of memory\"}\n";
		(void)!write(STDOUT_FILENO, oom, sizeof(oom) - 1);
		_exit(1);
	}
	b->data = data;
	b->cap = cap;
}

static void buf_put(struct buf *b, const char *s, size_t n)
{
	buf_reserve(b, n);
	memcpy(b->data + b->len, s, n);
	b->len += n;
	b->data[b->len] = '\0';
}

static void buf_puts(struct buf *b, const char *s) { buf_put(b, s, strlen(s)); }

static void buf_printf(struct buf *b, const char *fmt, ...)
{
	va_list ap;
	va_start(ap, fmt);
	int n = vsnprintf(NULL, 0, fmt, ap);
	va_end(ap);
	if (n < 0)
		return;
	buf_reserve(b, (size_t)n);
	va_start(ap, fmt);
	vsnprintf(b->data + b->len, (size_t)n + 1, fmt, ap);
	va_end(ap);
	b->len += (size_t)n;
}

/* Decode one UTF-8 sequence; returns its length, or 0 if invalid. */
static size_t utf8_decode(const unsigned char *s, size_t avail, uint32_t *cp)
{
	unsigned char c = s[0];
	size_t n;
	uint32_t v, min;
	if (c < 0x80) { *cp = c; return 1; }
	else if ((c & 0xE0) == 0xC0) { n = 2; v = c & 0x1F; min = 0x80; }
	else if ((c & 0xF0) == 0xE0) { n = 3; v = c & 0x0F; min = 0x800; }
	else if ((c & 0xF8) == 0xF0) { n = 4; v = c & 0x07; min = 0x10000; }
	else return 0;
	if (n > avail)
		return 0;
	for (size_t i = 1; i < n; i++) {
		if ((s[i] & 0xC0) != 0x80)
			return 0;
		v = (v << 6) | (s[i] & 0x3F);
	}
	if (v < min || v > 0x10FFFF || (v >= 0xD800 && v <= 0xDFFF))
		return 0;
	*cp = v;
	return n;
}

/* A JSON string literal, ASCII-only (like Python's ensure_ascii=True). Invalid
 * UTF-8 bytes become U+FFFD so the output line is always valid JSON. */
static void json_string(struct buf *b, const char *s)
{
	buf_put(b, "\"", 1);
	if (!s)
		s = "";
	const unsigned char *p = (const unsigned char *)s;
	size_t avail = strlen(s);
	while (avail) {
		uint32_t cp;
		size_t n = utf8_decode(p, avail, &cp);
		if (n == 0) {
			cp = 0xFFFD;
			n = 1;
		}
		p += n;
		avail -= n;
		switch (cp) {
		case '"': buf_puts(b, "\\\""); break;
		case '\\': buf_puts(b, "\\\\"); break;
		case '\b': buf_puts(b, "\\b"); break;
		case '\f': buf_puts(b, "\\f"); break;
		case '\n': buf_puts(b, "\\n"); break;
		case '\r': buf_puts(b, "\\r"); break;
		case '\t': buf_puts(b, "\\t"); break;
		default:
			if (cp < 0x20 || (cp >= 0x7F && cp < 0x10000)) {
				buf_printf(b, "\\u%04x", (unsigned)cp);
			} else if (cp >= 0x10000) {
				cp -= 0x10000;
				buf_printf(b, "\\u%04x\\u%04x", (unsigned)(0xD800 + (cp >> 10)), (unsigned)(0xDC00 + (cp & 0x3FF)));
			} else {
				char ch = (char)cp;
				buf_put(b, &ch, 1);
			}
		}
	}
	buf_put(b, "\"", 1);
}

static pthread_mutex_t emit_lock = PTHREAD_MUTEX_INITIALIZER;

/* Writes one complete line; callers on the instproxy updater thread and the
 * main thread never interleave. Write errors are ignored (SIGPIPE is). */
static void emit(struct buf *b)
{
	buf_put(b, "\n", 1);
	pthread_mutex_lock(&emit_lock);
	size_t off = 0;
	while (off < b->len) {
		ssize_t n = write(STDOUT_FILENO, b->data + off, b->len - off);
		if (n < 0 && errno == EINTR)
			continue;
		if (n <= 0)
			break;
		off += (size_t)n;
	}
	pthread_mutex_unlock(&emit_lock);
	free(b->data);
	b->data = NULL;
	b->len = b->cap = 0;
}

static void emit_error(const char *fmt, ...)
{
	char message[1024];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(message, sizeof(message), fmt, ap);
	va_end(ap);
	struct buf b = {0};
	buf_puts(&b, "{\"error\":");
	json_string(&b, message);
	buf_puts(&b, "}");
	emit(&b);
}

/* ---------- small helpers ---------- */

static int64_t now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void sleep_ms(int ms)
{
	struct timespec ts = { ms / 1000, (long)(ms % 1000) * 1000000L };
	while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
	}
}

/* Caller frees. NULL if the node is absent or not a string. */
static char *dict_string(plist_t dict, const char *key)
{
	plist_t node = dict ? plist_dict_get_item(dict, key) : NULL;
	char *value = NULL;
	if (node && plist_get_node_type(node) == PLIST_STRING)
		plist_get_string_val(node, &value);
	return value;
}

/* ---------- session ---------- */

struct session {
	idevice_t device;
	lockdownd_client_t lockdown;
};

static void session_close(struct session *s)
{
	if (s->lockdown) {
		lockdownd_client_free(s->lockdown);
		s->lockdown = NULL;
	}
	if (s->device) {
		idevice_free(s->device);
		s->device = NULL;
	}
}

/* The guest may still be bringing its USB stack up: retry for a bounded
 * window, then report the last failure. */
static int session_open(struct session *s)
{
	int64_t deadline = now_ms() + CONNECT_WINDOW_MS;
	const char *phase = "find guest";
	int code = 0;
	for (;;) {
		idevice_error_t derr = idevice_new_with_options(&s->device, NULL, IDEVICE_LOOKUP_USBMUX);
		if (derr == IDEVICE_E_SUCCESS) {
			lockdownd_error_t lerr = lockdownd_client_new_with_handshake(s->device, &s->lockdown, CLIENT_LABEL);
			if (lerr == LOCKDOWN_E_SUCCESS)
				return 0;
			phase = "lockdown handshake";
			code = lerr;
			s->lockdown = NULL;
			idevice_free(s->device);
			s->device = NULL;
		} else {
			phase = "find guest";
			code = derr;
			s->device = NULL;
		}
		if (now_ms() + CONNECT_RETRY_MS > deadline)
			break;
		sleep_ms(CONNECT_RETRY_MS);
	}
	emit_error("%s failed (device error %d). Try again after iOS finishes starting.", phase, code);
	return -1;
}

static char *lockdown_string(struct session *s, const char *key)
{
	plist_t node = NULL;
	lockdownd_error_t err = lockdownd_get_value(s->lockdown, NULL, key, &node);
	char *value = NULL;
	if (err == LOCKDOWN_E_SUCCESS && node && plist_get_node_type(node) == PLIST_STRING)
		plist_get_string_val(node, &value);
	if (node)
		plist_free(node);
	if (!value)
		emit_error("read %s failed (device error %d). Try again after iOS finishes starting.", key, (int)err);
	return value;
}

static int emit_target(struct session *s)
{
	char *product = lockdown_string(s, "ProductType");
	if (!product)
		return -1;
	char *version = lockdown_string(s, "ProductVersion");
	if (!version) {
		free(product);
		return -1;
	}
	struct buf b = {0};
	buf_puts(&b, "{\"target\":{\"product_type\":");
	json_string(&b, product);
	buf_puts(&b, ",\"ios\":");
	json_string(&b, version);
	buf_puts(&b, "}}");
	emit(&b);
	free(product);
	free(version);
	return 0;
}

static lockdownd_service_descriptor_t start_service(struct session *s, const char *name)
{
	lockdownd_service_descriptor_t service = NULL;
	lockdownd_error_t err = LOCKDOWN_E_UNKNOWN_ERROR;
	/* lockdownd drops a client left idle, as the first one is across a long AFC upload
	 * (start then fails with a mux error). A fresh handshake recovers it; the macOS
	 * pipeline's transient-connect retry handles the same thing. */
	for (int attempt = 0; attempt < 3; attempt++) {
		if (attempt) {
			if (s->lockdown) {
				lockdownd_client_free(s->lockdown);
				s->lockdown = NULL;
			}
			sleep_ms(500 * attempt);
			err = lockdownd_client_new_with_handshake(s->device, &s->lockdown, CLIENT_LABEL);
			if (err != LOCKDOWN_E_SUCCESS) {
				s->lockdown = NULL;
				continue;
			}
		}
		err = lockdownd_start_service(s->lockdown, name, &service);
		if (err == LOCKDOWN_E_SUCCESS && service)
			return service;
		if (service) {
			lockdownd_service_descriptor_free(service);
			service = NULL;
		}
	}
	emit_error("start %s failed (device error %d). Try again after iOS finishes starting.", name, (int)err);
	return NULL;
}

static instproxy_client_t instproxy_open(struct session *s)
{
	lockdownd_service_descriptor_t service = start_service(s, INSTPROXY_SERVICE_NAME);
	if (!service)
		return NULL;
	instproxy_client_t client = NULL;
	instproxy_error_t err = instproxy_client_new(s->device, service, &client);
	lockdownd_service_descriptor_free(service);
	if (err != INSTPROXY_E_SUCCESS) {
		emit_error("connect %s failed (device error %d). Try again after iOS finishes starting.",
		           INSTPROXY_SERVICE_NAME, (int)err);
		return NULL;
	}
	return client;
}

/* ---------- bounded diagnostic syslog relay ---------- */

static int64_t monotonic_ns(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static int action_syslog(struct session *s, unsigned seconds)
{
	lockdownd_service_descriptor_t service = start_service(s, SYSLOG_RELAY_SERVICE_NAME);
	if (!service)
		return 1;
	syslog_relay_client_t client = NULL;
	syslog_relay_error_t err = syslog_relay_client_new(s->device, service, &client);
	lockdownd_service_descriptor_free(service);
	if (err != SYSLOG_RELAY_E_SUCCESS) {
		emit_error("connect syslog relay failed (device error %d).", (int)err);
		return 1;
	}
	int64_t started = monotonic_ns();
	int64_t deadline = started + (int64_t)seconds * 1000000000;
	uint64_t chunks = 0, bytes = 0;
	struct buf b = {0};
	buf_printf(&b, "{\"syslog_start\":{\"monotonic_ns\":%" PRId64 ",\"seconds\":%u}}", started, seconds);
	emit(&b);
	int rc = 0;
	for (;;) {
		int64_t before = monotonic_ns();
		if (before >= deadline)
			break;
		unsigned timeout = (unsigned)((deadline - before + 999999) / 1000000);
		if (timeout > 250)
			timeout = 250;
		char data[8192];
		uint32_t got = 0;
		err = syslog_relay_receive_with_timeout(client, data, sizeof(data), &got, timeout);
		int64_t after = monotonic_ns();
		if (got) {
			/* Hex preserves NUL separators and arbitrary bytes exactly. Receive
			 * bounds use the same Android monotonic clock as frame logs, but are
			 * transport times, not guest message execution timestamps. */
			buf_printf(&b, "{\"syslog_chunk\":{\"begin_ns\":%" PRId64 ",\"end_ns\":%" PRId64
			           ",\"bytes\":%u,\"hex\":\"", before, after, got);
			static const char hex[] = "0123456789abcdef";
			for (uint32_t i = 0; i < got; i++) {
				unsigned char c = (unsigned char)data[i];
				char pair[] = { hex[c >> 4], hex[c & 15] };
				buf_put(&b, pair, 2);
			}
			buf_puts(&b, "\"}}");
			emit(&b);
			chunks++;
			bytes += got;
		}
		if (err != SYSLOG_RELAY_E_SUCCESS && err != SYSLOG_RELAY_E_TIMEOUT &&
		    err != SYSLOG_RELAY_E_NOT_ENOUGH_DATA) {
			emit_error("receive syslog failed (device error %d); capture incomplete.", (int)err);
			rc = 1;
			break;
		}
		if (!got && err == SYSLOG_RELAY_E_SUCCESS) {
			emit_error("syslog relay closed; capture incomplete.");
			rc = 1;
			break;
		}
	}
	syslog_relay_client_free(client);
	if (!rc) {
		buf_printf(&b, "{\"result\":{\"complete\":true,\"begin_ns\":%" PRId64
		           ",\"end_ns\":%" PRId64 ",\"chunks\":%" PRIu64 ",\"bytes\":%" PRIu64 "}}",
		           started, monotonic_ns(), chunks, bytes);
		emit(&b);
	}
	return rc;
}

/* Browse User applications on a fresh instproxy client. Caller frees *apps. */
static int browse_user_apps(struct session *s, plist_t *apps)
{
	instproxy_client_t client = instproxy_open(s);
	if (!client)
		return -1;
	plist_t options = plist_new_dict();
	plist_dict_set_item(options, "ApplicationType", plist_new_string("User"));
	plist_t result = NULL;
	instproxy_error_t err = instproxy_browse(client, options, &result);
	plist_free(options);
	instproxy_client_free(client);
	if (err != INSTPROXY_E_SUCCESS || !result || plist_get_node_type(result) != PLIST_ARRAY) {
		if (result)
			plist_free(result);
		emit_error("list installed apps failed (device error %d).", (int)err);
		return -1;
	}
	*apps = result;
	return 0;
}

/* ---------- list ---------- */

/* Newest crash report (by name, which embeds the date), for diagnosing an app that quits on
 * launch. iOS keeps them in the crash-report copy service's AFC root; LatestCrash links skipped. */
static int action_crashlog(struct session *s)
{
	lockdownd_service_descriptor_t service = start_service(s, "com.apple.crashreportcopymobile");
	if (!service)
		return 1;
	afc_client_t afc = NULL;
	afc_error_t err = afc_client_new(s->device, service, &afc);
	lockdownd_service_descriptor_free(service);
	if (err != AFC_E_SUCCESS) {
		emit_error("connect crash reports failed (device error %d).", (int)err);
		return 1;
	}
	char **list = NULL;
	int rc = 1;
	if (afc_read_directory(afc, "/", &list) != AFC_E_SUCCESS || !list) {
		emit_error("list crash reports failed.");
		goto done;
	}
	const char *newest = NULL;
	for (int i = 0; list[i]; i++) {
		const char *n = list[i];
		size_t len = strlen(n);
		int report = (len > 6 && strcmp(n + len - 6, ".plist") == 0) || (len > 4 && strcmp(n + len - 4, ".ips") == 0) ||
		             (len > 6 && strcmp(n + len - 6, ".crash") == 0);
		if (n[0] == '.' || strncmp(n, "LatestCrash", 11) == 0 || !report)
			continue;
		/* Names end in _YYYY-MM-DD-HHMMSS_device.ext: compare from the first '_2'. */
		const char *d = strstr(n, "_20"), *nd = newest ? strstr(newest, "_20") : NULL;
		if (!newest || (d && (!nd || strcmp(d, nd) > 0)))
			newest = n;
	}
	struct buf b = {0};
	if (!newest) {
		buf_puts(&b, "{\"result\":{\"name\":null}}");
		emit(&b);
		rc = 0;
		goto done;
	}
	char path[1024];
	snprintf(path, sizeof path, "/%s", newest);
	uint64_t handle = 0;
	if (afc_file_open(afc, path, AFC_FOPEN_RDONLY, &handle) != AFC_E_SUCCESS) {
		emit_error("open crash report failed.");
		goto done;
	}
	struct buf text = {0};
	char chunk[16384];
	uint32_t got = 0;
	while (text.len < (256u << 10) && afc_file_read(afc, handle, chunk, sizeof chunk, &got) == AFC_E_SUCCESS && got > 0)
		buf_put(&text, chunk, got);
	afc_file_close(afc, handle);
	buf_put(&text, "", 1);
	buf_puts(&b, "{\"result\":{\"name\":");
	json_string(&b, newest);
	buf_puts(&b, ",\"text\":");
	json_string(&b, text.data ? text.data : "");
	buf_puts(&b, "}}");
	free(text.data);
	emit(&b);
	rc = 0;
done:
	if (list)
		afc_dictionary_free(list);
	afc_client_free(afc);
	return rc;
}

static int action_list(struct session *s)
{
	plist_t apps = NULL;
	if (browse_user_apps(s, &apps) != 0)
		return 1;
	struct buf b = {0};
	buf_puts(&b, "{\"result\":{\"apps\":[");
	uint32_t count = plist_array_get_size(apps), written = 0;
	for (uint32_t i = 0; i < count; i++) {
		plist_t app = plist_array_get_item(apps, i);
		if (!app || plist_get_node_type(app) != PLIST_DICT)
			continue;
		char *id = dict_string(app, "CFBundleIdentifier");
		if (!id)
			continue;
		char *name = dict_string(app, "CFBundleDisplayName");
		if (!name)
			name = dict_string(app, "CFBundleName");
		char *version = dict_string(app, "CFBundleVersion");
		char *short_version = dict_string(app, "CFBundleShortVersionString");
		if (written++)
			buf_puts(&b, ",");
		buf_puts(&b, "{\"bundle_id\":");
		json_string(&b, id);
		buf_puts(&b, ",\"name\":");
		if (name) json_string(&b, name); else buf_puts(&b, "null");
		buf_puts(&b, ",\"version\":");
		if (version) json_string(&b, version); else buf_puts(&b, "null");
		if (short_version) {
			buf_puts(&b, ",\"short_version\":");
			json_string(&b, short_version);
		}
		buf_puts(&b, "}");
		free(id); free(name); free(version); free(short_version);
	}
	buf_puts(&b, "]}}");
	plist_free(apps);
	emit(&b);
	return 0;
}

/* ---------- install ---------- */

static uint16_t le16(const unsigned char *p) { return (uint16_t)(p[0] | p[1] << 8); }
static uint32_t le32(const unsigned char *p) { return (uint32_t)le16(p) | (uint32_t)le16(p + 2) << 16; }
static uint64_t le64(const unsigned char *p) { return (uint64_t)le32(p) | (uint64_t)le32(p + 4) << 32; }

/* The IPA's files once unpacked, from its zip central directory (zip64 too); 0 if unreadable. */
static uint64_t unpacked_size(int fd, uint64_t file_size)
{
	unsigned char tail[65557];
	size_t want = file_size < sizeof tail ? (size_t)file_size : sizeof tail;
	if (want < 22 || pread(fd, tail, want, (off_t)(file_size - want)) != (ssize_t)want)
		return 0;
	ssize_t e = (ssize_t)want - 22;
	while (e >= 0 && le32(tail + e) != 0x06054b50)
		e--;
	if (e < 0)
		return 0;
	uint64_t entries = le16(tail + e + 10), cd_size = le32(tail + e + 12), cd_off = le32(tail + e + 16);
	if ((entries == 0xffff || cd_size == 0xffffffffu || cd_off == 0xffffffffu) && e >= 20 &&
	    le32(tail + e - 20) == 0x07064b50) {
		unsigned char z[56];
		if (pread(fd, z, sizeof z, (off_t)le64(tail + e - 20 + 8)) != (ssize_t)sizeof z || le32(z) != 0x06064b50)
			return 0;
		entries = le64(z + 32);
		cd_size = le64(z + 40);
		cd_off = le64(z + 48);
	}
	if (cd_size > (64u << 20) || cd_off + cd_size > file_size)
		return 0;
	unsigned char *cd = malloc(cd_size ? cd_size : 1);
	if (!cd || pread(fd, cd, cd_size, (off_t)cd_off) != (ssize_t)cd_size) {
		free(cd);
		return 0;
	}
	uint64_t total = 0, pos = 0;
	for (uint64_t i = 0; i < entries && pos + 46 <= cd_size && le32(cd + pos) == 0x02014b50; i++) {
		uint64_t size = le32(cd + pos + 24);
		uint32_t name = le16(cd + pos + 28), extra = le16(cd + pos + 30), comment = le16(cd + pos + 32);
		if (pos + 46 + name + extra > cd_size)
			break;
		if (size == 0xffffffffu) {      /* zip64 extra field: the uncompressed size comes first */
			for (uint32_t x = 0; x + 4 <= extra;) {
				const unsigned char *f = cd + pos + 46 + name + x;
				uint32_t flen = le16(f + 2);
				if (le16(f) == 1 && flen >= 8 && x + 4 + flen <= extra) {
					size = le64(f + 4);
					break;
				}
				x += 4 + flen;
			}
		}
		total += size;
		pos += 46 + name + extra + comment;
	}
	free(cd);
	return total;
}

static int free_bytes(afc_client_t afc, uint64_t *out)
{
	char *text = NULL, *end = NULL;
	if (afc_get_device_info_key(afc, "FSFreeBytes", &text) != AFC_E_SUCCESS || !text) {
		free(text);
		return -1;
	}
	errno = 0;
	unsigned long long v = strtoull(text, &end, 10);
	int bad = errno || end == text;
	free(text);
	if (bad)
		return -1;
	*out = v;
	return 0;
}

struct install_state {
	pthread_mutex_t lock;
	pthread_cond_t cond;
	int terminal;
	char *error;
	int64_t last_ms;
	int extracting;         /* the last status was ExtractingPackage */
	int64_t percent;        /* its PercentComplete, -1 if none */
};

static void install_callback(plist_t command, plist_t status, void *user_data)
{
	(void)command;
	struct install_state *st = user_data;
	char *error = NULL, *state = NULL;
	int terminal = 0;
	if (!status || plist_get_node_type(status) != PLIST_DICT) {
		error = strdup("Invalid install callback: not a dictionary");
		terminal = 1;
	} else if (plist_dict_get_item(status, "Error")) {
		error = dict_string(status, "ErrorDescription");
		if (!error)
			error = dict_string(status, "Error");
		if (!error)
			error = strdup("The guest reported an install error.");
		terminal = 1;
	} else {
		state = dict_string(status, "Status");
		if (state && strcmp(state, "Complete") == 0) {
			terminal = 1;
		} else {
			plist_t node = plist_dict_get_item(status, "PercentComplete");
			uint64_t percent = 0;
			int have_percent = 0;
			if (node && plist_get_node_type(node) == PLIST_INT) {
				plist_get_uint_val(node, &percent);
				have_percent = 1;
			}
			struct buf b = {0};
			buf_puts(&b, "{\"phase\":\"install\",\"status\":");
			json_string(&b, state ? state : "Installing");
			if (have_percent)
				buf_printf(&b, ",\"percent\":%" PRIu64 "}", percent);
			else
				buf_puts(&b, ",\"percent\":null}");
			emit(&b);
			pthread_mutex_lock(&st->lock);
			st->extracting = state && strcmp(state, "ExtractingPackage") == 0;
			st->percent = have_percent ? (int64_t)percent : -1;
			pthread_mutex_unlock(&st->lock);
		}
	}
	free(state);
	pthread_mutex_lock(&st->lock);
	st->last_ms = now_ms();
	if (terminal && !st->terminal) {
		st->terminal = 1;
		st->error = error;
		error = NULL;
	}
	pthread_cond_broadcast(&st->cond);
	pthread_mutex_unlock(&st->lock);
	free(error);
}

/* Submit and wait. Returns 0 on Complete; emits its own error otherwise.
 * The watchdog retires the process instead of freeing callback state under a
 * possibly blocked library thread (as Desktop's mobile.py does). */
static int run_install(instproxy_client_t client, const char *remote, afc_client_t afc, uint64_t unpacked_total)
{
	static struct install_state st; /* outlives any updater thread */
	pthread_mutex_init(&st.lock, NULL);
	pthread_condattr_t attr;
	pthread_condattr_init(&attr);
	pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
	pthread_cond_init(&st.cond, &attr);
	pthread_condattr_destroy(&attr);
	st.terminal = 0;
	st.error = NULL;
	st.last_ms = now_ms();
	st.extracting = 0;
	st.percent = -1;
	/* iOS reports nothing while it unpacks the app, which takes minutes for a large one on a
	 * phone. The guest's free space falls as the files land: that is the progress, and any
	 * change counts as activity. The overall limit grows with the app. */
	uint64_t free_start = 0, free_last = 0;
	int have_space = free_bytes(afc, &free_start) == 0;
	free_last = free_start;
	int64_t next_poll = now_ms() + SPACE_POLL_MS;

	struct buf b = {0};
	buf_puts(&b, "{\"phase\":\"mutation\",\"status\":\"Installing; wait for completion before stopping iOS\"}");
	emit(&b);

	plist_t options = plist_new_dict();
	instproxy_error_t err = instproxy_install(client, remote, options, install_callback, &st);
	plist_free(options);
	if (err != INSTPROXY_E_SUCCESS) {
		emit_error("submit install failed (device error %d). Try again after iOS finishes starting.", (int)err);
		return -1;
	}
	int64_t deadline = now_ms() + (int64_t)INSTALL_ABSOLUTE_S * 1000 +
	                   (int64_t)(unpacked_total / UNPACK_FLOOR_BPS) * 1000;
	pthread_mutex_lock(&st.lock);
	while (!st.terminal) {
		struct timespec ts;
		clock_gettime(CLOCK_MONOTONIC, &ts);
		ts.tv_nsec += 200 * 1000000L;
		if (ts.tv_nsec >= 1000000000L) {
			ts.tv_sec++;
			ts.tv_nsec -= 1000000000L;
		}
		pthread_cond_timedwait(&st.cond, &st.lock, &ts);
		if (st.terminal)
			break;
		int64_t now = now_ms();
		if (have_space && now >= next_poll) {
			next_poll = now + SPACE_POLL_MS;
			int extracting = st.extracting;
			int64_t percent = st.percent;
			pthread_mutex_unlock(&st.lock);
			uint64_t free_now = free_last;
			if (free_bytes(afc, &free_now) == 0 && free_now != free_last) {
				free_last = free_now;
				if (extracting && unpacked_total) {
					uint64_t done = free_start > free_now ? free_start - free_now : 0;
					struct buf b = {0};
					buf_puts(&b, "{\"phase\":\"install\",\"status\":\"ExtractingPackage\",\"percent\":");
					if (percent >= 0)
						buf_printf(&b, "%" PRId64, percent);
					else
						buf_puts(&b, "null");
					buf_printf(&b, ",\"unpacked\":%" PRIu64 ",\"unpacked_total\":%" PRIu64 "}",
					           done < unpacked_total ? done : unpacked_total, unpacked_total);
					emit(&b);
				}
				pthread_mutex_lock(&st.lock);
				st.last_ms = now;
			} else {
				pthread_mutex_lock(&st.lock);
			}
			if (st.terminal)
				break;
		}
		if (now >= deadline || now - st.last_ms > (int64_t)INSTALL_IDLE_S * 1000) {
			pthread_mutex_unlock(&st.lock);
			emit_error("Install timed out; guest result is unconfirmed. Inspect before retrying.");
			_exit(2);
		}
	}
	char *error = st.error;
	st.error = NULL;
	pthread_mutex_unlock(&st.lock);
	if (error) {
		emit_error("%s", error);
		free(error);
		return -1;
	}
	return 0;
}

static int afc_check(afc_error_t err, const char *phase)
{
	if (err == AFC_E_SUCCESS)
		return 0;
	emit_error("%s failed (device error %d). Try again after iOS finishes starting.", phase, (int)err);
	return -1;
}

static int action_install(struct session *s, const char *ipa, const char *bundle_id)
{
	int fd = open(ipa, O_RDONLY | O_CLOEXEC);
	struct stat st;
	if (fd < 0 || fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0) {
		emit_error("The IPA cannot be read: %s", fd < 0 ? strerror(errno) : "not a non-empty regular file");
		if (fd >= 0)
			close(fd);
		return 1;
	}
	uint64_t total = (uint64_t)st.st_size;

	lockdownd_service_descriptor_t service = start_service(s, AFC_SERVICE_NAME);
	if (!service) {
		close(fd);
		return 1;
	}
	afc_client_t afc = NULL;
	afc_error_t aerr = afc_client_new(s->device, service, &afc);
	lockdownd_service_descriptor_free(service);
	if (aerr != AFC_E_SUCCESS) {
		emit_error("connect %s failed (device error %d). Try again after iOS finishes starting.", AFC_SERVICE_NAME, (int)aerr);
		close(fd);
		return 1;
	}

	unsigned char random[16];
	arc4random_buf(random, sizeof(random));
	char remote[96];
	int pos = snprintf(remote, sizeof(remote), STAGING_DIR "/lighttouch-");
	for (size_t i = 0; i < sizeof(random); i++)
		pos += snprintf(remote + pos, sizeof(remote) - (size_t)pos, "%02x", random[i]);
	snprintf(remote + pos, sizeof(remote) - (size_t)pos, ".ipa");

	int rc = 1;
	uint64_t handle = 0;
	int opened = 0;
	char *buffer = NULL;
	instproxy_client_t client = NULL;
	plist_t apps = NULL;

	char *free_text = NULL;
	if (afc_check(afc_get_device_info_key(afc, "FSFreeBytes", &free_text), "query free space") != 0)
		goto out;
	char *end = NULL;
	errno = 0;
	unsigned long long available = free_text ? strtoull(free_text, &end, 10) : 0;
	int bad = !free_text || errno || end == free_text;
	free(free_text);
	if (bad) {
		emit_error("The guest returned invalid free-space metadata.");
		goto out;
	}
	uint64_t needed = total * 2 + (16ull << 20);
	if (available < needed) {
		emit_error("The guest needs %" PRIu64 " MiB free; it has %llu MiB.", needed >> 20, available >> 20);
		goto out;
	}
	/* Existing PublicStaging is expected; mkdir's exists result is harmless. */
	afc_make_directory(afc, STAGING_DIR);
	if (afc_check(afc_file_open(afc, remote, AFC_FOPEN_WRONLY, &handle), "open staging upload") != 0)
		goto out;
	opened = 1;

	buffer = malloc(UPLOAD_CHUNK);
	if (!buffer) {
		emit_error("out of memory");
		goto out;
	}
	uint64_t sent = 0;
	int last_percent = -1;
	for (;;) {
		ssize_t got = read(fd, buffer, UPLOAD_CHUNK);
		if (got < 0 && errno == EINTR)
			continue;
		if (got < 0) {
			emit_error("Reading the IPA failed: %s", strerror(errno));
			goto out;
		}
		if (got == 0)
			break;
		uint32_t offset = 0;
		while (offset < (uint32_t)got) {
			uint32_t written = 0;
			uint32_t remaining = (uint32_t)got - offset;
			if (afc_check(afc_file_write(afc, handle, buffer + offset, remaining, &written), "upload IPA") != 0)
				goto out;
			if (written == 0 || written > remaining) {
				emit_error("The guest returned an invalid upload length.");
				goto out;
			}
			offset += written;
			sent += written;
		}
		if (sent > total) {
			emit_error("The IPA changed while it was being uploaded.");
			goto out;
		}
		int percent = (int)(sent * 100 / total);
		if (percent != last_percent) {
			struct buf b = {0};
			buf_printf(&b, "{\"phase\":\"upload\",\"percent\":%d}", percent);
			emit(&b);
			last_percent = percent;
		}
	}
	if (sent != total) {
		emit_error("The IPA changed while it was being uploaded.");
		goto out;
	}
	opened = 0;
	if (afc_check(afc_file_close(afc, handle), "close staging upload") != 0)
		goto out;

	client = instproxy_open(s);
	if (!client)
		goto out;
	int installed = run_install(client, remote, afc, unpacked_size(fd, total));
	/* Join the updater thread before anything else; browse on a fresh client
	 * (the old updater may still own its command when Complete wakes us). */
	instproxy_client_free(client);
	client = NULL;
	if (installed != 0)
		goto out;

	/* A callback alone is insufficient: independently query the installed list. */
	if (browse_user_apps(s, &apps) != 0)
		goto out;
	plist_t found = NULL;
	uint32_t count = plist_array_get_size(apps);
	for (uint32_t i = 0; i < count && !found; i++) {
		plist_t app = plist_array_get_item(apps, i);
		char *id = dict_string(app, "CFBundleIdentifier");
		if (id && strcmp(id, bundle_id) == 0)
			found = app;
		free(id);
	}
	if (!found) {
		emit_error("Install reported Complete but the app is absent from the guest's app list.");
		goto out;
	}
	char *version = dict_string(found, "CFBundleVersion");
	struct buf b = {0};
	buf_puts(&b, "{\"result\":{\"bundle_id\":");
	json_string(&b, bundle_id);
	buf_puts(&b, ",\"version\":");
	if (version) json_string(&b, version); else buf_puts(&b, "null");
	buf_puts(&b, ",\"installed\":true}}");
	free(version);
	emit(&b);
	rc = 0;

out:
	if (apps)
		plist_free(apps);
	if (client)
		instproxy_client_free(client);
	free(buffer);
	if (opened)
		afc_file_close(afc, handle);
	afc_remove_path(afc, remote);
	afc_client_free(afc);
	close(fd);
	return rc;
}

/* ---------- main ---------- */

/* Same contract as Desktop helper.py: an absolute, non-symlink socket owned by
 * us with no group/other bits, in a directory that is also ours alone. */
static int check_socket(const char *path)
{
	struct sockaddr_un probe;
	if (!path || path[0] != '/' || strlen(path) >= sizeof(probe.sun_path)) {
		emit_error("A private owner-only guest usbmux socket is required.");
		return -1;
	}
	struct stat info, parent;
	char *copy = strdup(path);
	if (!copy) {
		emit_error("out of memory");
		return -1;
	}
	int ok = lstat(path, &info) == 0 && S_ISSOCK(info.st_mode) && info.st_uid == geteuid() &&
	         (info.st_mode & 077) == 0 && stat(dirname(copy), &parent) == 0 && S_ISDIR(parent.st_mode) &&
	         parent.st_uid == geteuid() && (parent.st_mode & 077) == 0;
	free(copy);
	if (!ok) {
		emit_error("A private owner-only guest usbmux socket is required.");
		return -1;
	}
	return 0;
}

static void parent_death_guard(void)
{
	const char *enable = getenv("LT_PARENT_DEATH_SIGNAL");
	if (!enable || strcmp(enable, "1") != 0)
		return;
	if (prctl(PR_SET_PDEATHSIG, SIGTERM) != 0)
		_exit(1);
	const char *expected = getenv("LT_PARENT_PID");
	pid_t parent = getppid();
	if (parent == 1 || (expected && *expected && strtol(expected, NULL, 10) != (long)parent))
		_exit(1);
}

static void usage_error(const char *detail)
{
	emit_error("%s Usage: info|list|crashlog --socket PATH, install --socket PATH --ipa PATH --bundle-id ID, or syslog --socket PATH --seconds 1..600", detail);
}

int main(int argc, char **argv)
{
	parent_death_guard();
	signal(SIGPIPE, SIG_IGN);

	if (argc < 2) {
		usage_error("Missing action.");
		return 1;
	}
	const char *action = argv[1];
	const char *socket_path = NULL, *ipa = NULL, *bundle_id = NULL, *seconds_text = NULL;
	int verbose = 0;
	for (int i = 2; i < argc; i++) {
		const char **slot = NULL;
		if (strcmp(argv[i], "--socket") == 0)
			slot = &socket_path;
		else if (strcmp(argv[i], "--ipa") == 0)
			slot = &ipa;
		else if (strcmp(argv[i], "--bundle-id") == 0)
			slot = &bundle_id;
		else if (strcmp(argv[i], "--seconds") == 0)
			slot = &seconds_text;
		else if (strcmp(argv[i], "--verbose") == 0) {
			verbose = 1;
			continue;
		} else {
			usage_error("Unknown argument.");
			return 1;
		}
		if (i + 1 >= argc || *slot) {
			usage_error("Missing or repeated option value.");
			return 1;
		}
		*slot = argv[++i];
	}
	int is_install = strcmp(action, "install") == 0;
	int is_syslog = strcmp(action, "syslog") == 0;
	if (!is_install && !is_syslog && strcmp(action, "info") != 0 && strcmp(action, "list") != 0 && strcmp(action, "crashlog") != 0) {
		usage_error("Unknown action.");
		return 1;
	}
	if (!socket_path) {
		usage_error("--socket is required.");
		return 1;
	}
	if (is_install && (!ipa || !*ipa)) {
		emit_error("Choose an IPA.");
		return 1;
	}
	if (is_install && (!bundle_id || !*bundle_id)) {
		emit_error("The app's bundle identifier is required.");
		return 1;
	}
	if (!is_install && (ipa || bundle_id)) {
		usage_error("--ipa/--bundle-id apply to install only.");
		return 1;
	}
	unsigned seconds = 0;
	if (is_syslog) {
		char *end = NULL;
		errno = 0;
		unsigned long value = seconds_text ? strtoul(seconds_text, &end, 10) : 0;
		if (!seconds_text || !*seconds_text || *seconds_text < '0' || *seconds_text > '9' ||
		    errno || !end || *end || value < 1 || value > 600) {
			usage_error("syslog requires --seconds 1..600.");
			return 1;
		}
		seconds = (unsigned)value;
	} else if (seconds_text) {
		usage_error("--seconds applies to syslog only.");
		return 1;
	}
	if (check_socket(socket_path) != 0)
		return 1;

	/* Only this daemon: never a system usbmuxd or a network device. */
	char address[sizeof(((struct sockaddr_un *)0)->sun_path) + 8];
	snprintf(address, sizeof(address), "UNIX:%s", socket_path);
	if (setenv("USBMUXD_SOCKET_ADDRESS", address, 1) != 0) {
		emit_error("Could not select the guest usbmux socket.");
		return 1;
	}
	if (verbose)
		idevice_set_debug_level(1);

	struct session s = {0};
	if (session_open(&s) != 0)
		return 1;
	int rc = 1;
	if (emit_target(&s) == 0) {
		if (is_install) {
			rc = action_install(&s, ipa, bundle_id);
		} else if (strcmp(action, "list") == 0) {
			rc = action_list(&s);
		} else if (strcmp(action, "crashlog") == 0) {
			rc = action_crashlog(&s);
		} else if (is_syslog) {
			rc = action_syslog(&s, seconds);
		} else {
			struct buf b = {0};
			buf_puts(&b, "{\"result\":{\"ready\":true}}");
			emit(&b);
			rc = 0;
		}
	}
	session_close(&s);
	return rc;
}
