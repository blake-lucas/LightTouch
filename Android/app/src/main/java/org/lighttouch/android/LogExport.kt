package org.lighttouch.android

import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.content.Context
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.GLES20
import android.os.Build
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.RandomAccessFile
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/**
 * Settings › Diagnostics › Export logs, and Save logs on the preparation pages: the logs of docs/LOGS.md and what
 * explains them, in one zip for a bug report. Everything in it is text, with the app's private paths scrubbed.
 */
internal object LogExport {
    /** What only the activity knows: the devices, the guest's state, and the guest's newest crash report (or null). */
    class Inputs(val devices: List<DeviceSummary>, val status: String, val lastCrash: () -> Pair<String, String>?)

    private const val SERIAL_TAIL = Logs.MIB
    private const val LOGCAT_TAIL = Logs.MIB
    private const val TRACE_TAIL = Logs.MIB

    fun build(context: Context, inputs: Inputs): File {
        val time = Date()
        val name = "LightTouch-logs-${SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(time)}.zip"
        val dir = File(context.cacheDir, "log-exports").apply { listFiles()?.forEach { it.delete() }; mkdirs() }
        val zip = File(dir, name)
        val logs = Logs.dir(context)
        val scrub = scrubber(context)
        val blank = blanker(context)
        val listed = mutableListOf<Pair<String, Int>>()
        val missing = mutableListOf<String>()
        val entries = linkedMapOf<String, ByteArray>()
        fun add(entry: String, bytes: ByteArray?, why: String, binary: Boolean = false) {
            if (bytes == null || bytes.isEmpty()) { missing += "$entry ($why)"; return }
            entries[entry] = if (binary) blank(bytes) else scrub(String(bytes, Charsets.UTF_8)).toByteArray()
        }
        fun log(name: String, previous: Boolean) =
            Logs.files(logs, name, previous).takeIf { it.isNotEmpty() }?.let { ByteArrayOutputStream().also { out -> Logs.copyTo(it, out) }.toByteArray() }

        add("app.log", log("app", false), "empty")
        add("app-previous.log", log("app", true), "LightTouch hasn't opened before this run since the update")
        add("engine.log", log("engine", false), "no device has started since the update")
        add("engine-previous.log", log("engine", true), "no earlier device start")
        add("guest-serial.log", tail(File(logs, "guest-serial.log"), SERIAL_TAIL), "no device has started since the update")
        add("guest-serial-previous.log", tail(File(logs, "guest-serial.prev.log"), SERIAL_TAIL), "no earlier device start")
        add("prepare.log", log("prepare", false), "no device prepared since the update")
        add("prepare-previous.log", log("prepare", true), "no earlier preparation")
        val crash = if (inputs.status in listOf("running", "paused")) runCatching { inputs.lastCrash() }.getOrNull() else null
        add("guest/last-crash.txt", crash?.let { "${it.first}\n\n${it.second}".toByteArray() },
            if (inputs.status in listOf("running", "paused")) "iOS has no crash report, or its guest tools didn't answer" else "the device wasn't running")
        inputs.devices.firstOrNull { it.selected }?.let { device ->
            val record = File(context.filesDir, "library/records/${device.id}")
            add("device/record.json", File(record, "record.json").takeIf { it.isFile }?.readBytes(), "missing")
            add("device/device.lock.json", File(record, "base/device.lock.json").takeIf { it.isFile }?.readBytes(), "missing")
        }
        File(context.filesDir, "service-logs").listFiles()?.maxByOrNull { it.lastModified() }?.let { add("usbmuxd.log", tail(it, SERIAL_TAIL), "empty") }
        add("android/logcat.txt", logcat(), "Android gave no log")
        add("android/exits.txt", exits(context)?.toByteArray(), "Android keeps no exit history before Android 11")
        File(context.filesDir, "worker-exits").listFiles { f -> f.name.endsWith(".trace") }?.sortedByDescending { it.lastModified() }?.take(3)?.forEach {
            // A trace is Android's tombstone protobuf: its bytes go in as they are (capped at capture).
            add("android/exits/${it.name}", it.takeIf { f -> f.length() <= TRACE_TAIL }?.readBytes(), "empty", binary = true)
        }

        val summary = summary(context, inputs, time)
        ZipOutputStream(zip.outputStream().buffered()).use { out ->
            val files = entries.map { (entry, bytes) -> entry to bytes.size }
            listed += files
            val text = buildString {
                append(summary)
                appendLine(); appendLine("Files")
                for ((entry, size) in listed) appendLine("  %-32s %s".format(entry, size(size.toLong())))
                if (missing.isNotEmpty()) { appendLine(); appendLine("Not included"); missing.forEach { appendLine("  $it") } }
            }
            out.putNextEntry(ZipEntry("summary.txt")); out.write(scrub(text).toByteArray()); out.closeEntry()
            for ((entry, bytes) in entries) { out.putNextEntry(ZipEntry(entry)); out.write(bytes); out.closeEntry() }
        }
        AppLog.i("Logs exported: ${zip.name}, ${size(zip.length())}")
        return zip
    }

    private fun summary(context: Context, inputs: Inputs, time: Date) = buildString {
        appendLine("LightTouch logs, exported ${SimpleDateFormat("yyyy-MM-dd HH:mm:ss Z", Locale.US).format(time)}")
        appendLine("See docs/LOGS.md in the LightTouch repository for what each file holds.")
        appendLine()
        appendLine("LightTouch ${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE}), commit ${BuildConfig.GIT_COMMIT}, ${if (BuildConfig.DEBUG) "debug" else "release"} build" +
            if (context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0) ", debuggable" else "")
        appendLine("Phone: ${Build.MANUFACTURER} ${Build.MODEL} (${Build.DEVICE}), Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT})")
        appendLine("Build: ${Build.FINGERPRINT}")
        appendLine("Chip: " + (if (Build.VERSION.SDK_INT >= 31) "${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}" else Build.HARDWARE) +
            ", ${Runtime.getRuntime().availableProcessors()} cores, ${Build.SUPPORTED_ABIS.joinToString()}, page size ${android.system.Os.sysconf(android.system.OsConstants._SC_PAGESIZE)}")
        val memory = ActivityManager.MemoryInfo().also { context.getSystemService(ActivityManager::class.java).getMemoryInfo(it) }
        appendLine("Memory: ${size(memory.totalMem)} total, ${size(memory.availMem)} available${if (memory.lowMemory) " (low)" else ""}")
        appendLine("Storage: ${size(context.filesDir.usableSpace)} free of ${size(context.filesDir.totalSpace)}")
        gpuWithin(5_000).forEach { appendLine(it) }
        appendLine()
        appendLine("Settings")
        context.getSharedPreferences("ui", Context.MODE_PRIVATE).all.toSortedMap().forEach { (key, value) -> appendLine("  $key = $value") }
        val network = NetworkConfiguration.load(context)
        appendLine("  network = ${if (network.enabled) "on (${network.route.label}${if (network.route == NetworkRoute.ARCHIVE) " ${network.archiveDate}" else ""}, local network ${network.localNetwork})" else "off"}")
        appendLine()
        appendLine("Devices (${inputs.devices.size}); LightTouch's device state: ${inputs.status}")
        for (device in inputs.devices) {
            val lock = File(context.filesDir, "library/records/${device.id}/base/device.lock.json").takeIf { it.isFile }?.readText() ?: ""
            // The lock doesn't record the choice itself; a jailbroken system lists Cydia's bootstrap (as the desktop reads it).
            val jailbroken = lock.contains("cydia", ignoreCase = true) || lock.contains("\"jailbreak\"")
            val facts = runCatching { DeviceFacts.read(context, device.id) }.getOrNull()
            appendLine("  ${if (device.selected) "* " else "  "}\"${device.name}\": ${device.subtitle} (${device.board} ${device.build})" +
                ", jailbroken ${if (jailbroken) "yes" else "no"}" + (facts?.toolsSerial?.let { ", guest tools serial $it" } ?: "") +
                (if (device.dirty) ", left running (unclean)" else "") + ", record ${device.id.take(8)}")
        }
    }

    /** [gpu] on a thread of its own, so a driver that hangs (the suspect, when logs are wanted) can't stop the export. */
    private fun gpuWithin(millis: Long): List<String> {
        var result: List<String> = listOf("GPU: the driver didn't answer within ${millis / 1000} s")
        val probe = Thread({ val lines = gpu(); synchronized(this) { result = lines } }, "log-export-gpu").apply { isDaemon = true; start() }
        probe.join(millis)
        return synchronized(this) { result }
    }

    /** The GPU as LightTouch's own GL context sees it: the driver the engine draws with ("platform" graphics). */
    private fun gpu(): List<String> {
        val display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        return try {
            val version = IntArray(2)
            if (!EGL14.eglInitialize(display, version, 0, version, 1)) return listOf("GPU: EGL didn't initialize (0x%x)".format(EGL14.eglGetError()))
            val configs = arrayOfNulls<EGLConfig>(1); val count = IntArray(1)
            EGL14.eglChooseConfig(display, intArrayOf(EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT, EGL14.EGL_SURFACE_TYPE, EGL14.EGL_PBUFFER_BIT, EGL14.EGL_NONE), 0,
                configs, 0, 1, count, 0)
            val config = configs[0] ?: return listOf("GPU: no ES 2 pbuffer config")
            val context = EGL14.eglCreateContext(display, config, EGL14.EGL_NO_CONTEXT, intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0)
            val surface = EGL14.eglCreatePbufferSurface(display, config, intArrayOf(EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE), 0)
            try {
                if (!EGL14.eglMakeCurrent(display, surface, surface, context)) return listOf("GPU: make-current failed (0x%x)".format(EGL14.eglGetError()))
                listOf(
                    "GPU: ${GLES20.glGetString(GLES20.GL_VENDOR)} ${GLES20.glGetString(GLES20.GL_RENDERER)}",
                    "GL: ${GLES20.glGetString(GLES20.GL_VERSION)}, ${GLES20.glGetString(GLES20.GL_SHADING_LANGUAGE_VERSION)}",
                    "EGL: ${version[0]}.${version[1]} ${EGL14.eglQueryString(display, EGL14.EGL_VENDOR)} ${EGL14.eglQueryString(display, EGL14.EGL_VERSION)}",
                    "GL extensions: ${GLES20.glGetString(GLES20.GL_EXTENSIONS)?.trim()}",
                    "EGL extensions: ${EGL14.eglQueryString(display, EGL14.EGL_EXTENSIONS)?.trim()}",
                )
            } finally {
                EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
                EGL14.eglDestroySurface(display, surface); EGL14.eglDestroyContext(display, context)
            }
        } catch (e: Exception) { listOf("GPU: unavailable ($e)") }
        // No eglTerminate: the display is the process's one, which the UI's renderer may share.
    }

    /** LightTouch's own Android log (an app reads only its own): its processes, the GPU driver in them, crashes. */
    private fun logcat(): ByteArray? = try {
        val process = ProcessBuilder("logcat", "-d", "-v", "threadtime", "-b", "main,system,crash").redirectErrorStream(true).start()
        val ring = ArrayDeque<ByteArray>(); var held = 0L
        process.inputStream.use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val n = input.read(buffer); if (n < 0) break
                ring.addLast(buffer.copyOf(n)); held += n
                while (held - ring.first().size >= LOGCAT_TAIL) held -= ring.removeFirst().size
            }
        }
        process.waitFor()
        ByteArrayOutputStream().also { out -> ring.forEach { out.write(it) } }.toByteArray()
    } catch (_: Exception) { null }

    /** How LightTouch's processes ended lately, as Android recorded it. */
    private fun exits(context: Context): String? {
        if (Build.VERSION.SDK_INT < 30) return null
        val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US)
        return context.getSystemService(ActivityManager::class.java).getHistoricalProcessExitReasons(context.packageName, 0, 12)
            .joinToString("\n") { e ->
                "${stamp.format(Date(e.timestamp))}  ${e.processName} pid ${e.pid}: ${reason(e.reason)} (status ${e.status}, importance ${e.importance}, " +
                    "pss ${e.pss / 1024} MiB)${e.description?.takeIf { it.isNotBlank() }?.let { " — $it" } ?: ""}"
            }.ifEmpty { null }
    }

    private fun reason(reason: Int) = when (reason) {
        ApplicationExitInfo.REASON_EXIT_SELF -> "exited"; ApplicationExitInfo.REASON_SIGNALED -> "signaled"
        ApplicationExitInfo.REASON_LOW_MEMORY -> "low memory"; ApplicationExitInfo.REASON_CRASH -> "crashed (Java)"
        ApplicationExitInfo.REASON_CRASH_NATIVE -> "crashed (native)"; ApplicationExitInfo.REASON_ANR -> "not responding"
        ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> "failed to start"; ApplicationExitInfo.REASON_PERMISSION_CHANGE -> "permission change"
        ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> "excessive resource use"; ApplicationExitInfo.REASON_USER_REQUESTED -> "stopped by the user"
        ApplicationExitInfo.REASON_USER_STOPPED -> "user stopped"; ApplicationExitInfo.REASON_DEPENDENCY_DIED -> "dependency died"
        ApplicationExitInfo.REASON_OTHER -> "other"
        // Newer reasons, by number so older SDKs still name them: frozen (34), package state change and update (33).
        14 -> "frozen"; 15 -> "package state changed"; 16 -> "app updated"; else -> "reason $reason"
    }

    private fun tail(file: File, max: Long): ByteArray? {
        if (!file.isFile) return null
        return RandomAccessFile(file, "r").use { f ->
            val skip = (f.length() - max).coerceAtLeast(0L)
            f.seek(skip)
            val head = if (skip > 0) "----- [log] the last ${size(max)} of ${size(f.length())} -----\n".toByteArray() else ByteArray(0)
            head + ByteArray((f.length() - skip).toInt()).also { f.readFully(it) }
        }
    }

    /** The app's paths, which name the phone's user profile and the install, and their placeholders. */
    private fun scrubPairs(context: Context): List<Pair<String, String>> {
        val data = context.filesDir.parentFile?.path
        return listOfNotNull(
            data?.let { it to "<data>" },
            "/data/data/${context.packageName}" to "<data>",
            "/data/user/0/${context.packageName}" to "<data>",
            File(context.applicationInfo.sourceDir).parent?.let { it to "<apk>" },
            "/storage/emulated/0" to "<storage>",
        ).sortedByDescending { it.first.length }
    }

    /** The app's paths as placeholders. */
    private fun scrubber(context: Context): (String) -> String {
        val pairs = scrubPairs(context)
        return { text -> pairs.fold(text) { t, (from, to) -> t.replace(from, to) } }
    }

    /**
     * The same paths in a binary file, overwritten in place: the placeholder padded with '_' to the path's length.
     * A protobuf (an exit trace) keeps every length prefix valid; decoding it as text turned every byte >= 0x80
     * into U+FFFD and left the tombstone unreadable.
     */
    private fun blanker(context: Context): (ByteArray) -> ByteArray {
        val pairs = scrubPairs(context)
        return { input -> blankPaths(input, pairs) }
    }

    internal fun blankPaths(input: ByteArray, pairs: List<Pair<String, String>>): ByteArray {
        val out = input.copyOf()
        for ((from, to) in pairs) {
            val path = from.toByteArray()
            if (path.isEmpty()) continue
            val mask = to.toByteArray().copyOf(path.size).also { it.fill('_'.code.toByte(), minOf(to.length, path.size)) }
            var i = 0
            while (i <= out.size - path.size) {
                var match = true
                for (k in path.indices) if (out[i + k] != path[k]) { match = false; break }
                if (match) { mask.copyInto(out, i); i += path.size } else i++
            }
        }
        return out
    }

    private fun size(bytes: Long): String = when {
        bytes >= Logs.MIB * 1024 -> "%.1f GiB".format(bytes / (Logs.MIB * 1024.0))
        bytes >= Logs.MIB -> "%.1f MiB".format(bytes / Logs.MIB.toDouble())
        else -> "%.1f KiB".format(bytes / Logs.KIB.toDouble())
    }
}

/** Diagnostics that used to pile up, trimmed when LightTouch opens (docs/LOGS.md, Housekeeping). */
internal object Housekeeping {
    fun run(context: Context) = try {
        val files = context.filesDir
        File(files, "worker-exits").listFiles()?.sortedByDescending { it.lastModified() }?.drop(10)?.forEach { it.delete() }
        for (prefix in listOf("worker-observations-", "worker-shutdown-deadline-", "worker-shutdown-", "worker-stop-error-")) {
            files.listFiles { f -> f.isFile && f.name.startsWith(prefix) && (prefix != "worker-shutdown-" || !f.name.startsWith("worker-shutdown-deadline-")) }
                ?.sortedByDescending { it.lastModified() }?.drop(5)?.forEach { it.delete() }
        }
        File(files, "hitches").listFiles()?.sortedByDescending { it.lastModified() }?.drop(5)?.forEach { it.deleteRecursively() }
        File(context.cacheDir, "firmwarekit.log").delete()
        // An export waiting for its save dialog is kept: the activity may be recreated behind the dialog.
        File(context.cacheDir, "log-exports").listFiles()?.filter { System.currentTimeMillis() - it.lastModified() > 3_600_000 }?.forEach { it.deleteRecursively() }
        File(files, "library/records").listFiles()?.forEach { record ->
            File(record, "state").listFiles { f -> f.isFile && (f.name.startsWith("engine-") && f.name.endsWith(".log") || f.name == "guest-serial.log") }
                ?.forEach { it.delete() }
        }
    } catch (e: Exception) { AppLog.w("Housekeeping: $e") }
}
