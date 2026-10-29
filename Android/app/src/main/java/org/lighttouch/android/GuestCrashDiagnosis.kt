package org.lighttouch.android

import java.io.File
import java.io.RandomAccessFile

/**
 * The answer to "why did it quit", from the three places that know it.
 *
 * The crash report names the frame that faulted; the guest's system log says what led to it (dyld refusing a
 * symbol, a missing service, the watchdog); the engine log and the guest's serial log say whether the emulation
 * itself complained first. Together they tell an app that needs a newer iOS from an app our emulation broke,
 * which the report's first three lines alone cannot.
 */
internal object GuestCrashDiagnosis {
    private const val LOG_TAIL = 60

    /** The last [lines] lines of [file] without reading the whole thing: a record's log grows to megabytes. */
    fun tail(file: File?, lines: Int = LOG_TAIL, maxBytes: Long = 64L * 1024): List<String> {
        if (file == null || !file.isFile) return emptyList()
        return try {
            RandomAccessFile(file, "r").use { handle ->
                val length = handle.length()
                val start = (length - maxBytes).coerceAtLeast(0L)
                val bytes = ByteArray((length - start).toInt())
                handle.seek(start)
                handle.readFully(bytes)
                String(bytes, Charsets.UTF_8).lines().filter { it.isNotBlank() }.takeLast(lines).map { it.take(400) }
            }
        } catch (_: Exception) { emptyList() }
    }

    /** The record's newest engine log; the engine names them by worker pid. */
    fun engineLog(stateDir: File): File? =
        stateDir.listFiles { file -> file.isFile && file.name.startsWith("engine-") && file.name.endsWith(".log") }
            ?.maxByOrNull { it.lastModified() }

    /**
     * [crashName]/[crashBody] is the guest's newest crash report (null when it has none), [guestLog] the captured
     * system log of the last launch (null when nothing was captured). Everything else is read from [stateDir].
     */
    fun report(stateDir: File, crashName: String?, crashBody: String?, guestLog: String?): String {
        val summary = crashBody?.takeIf { it.isNotBlank() }?.let { GuestCrashReport.summarize(crashName ?: "", it) }
        val engine = tail(engineLog(stateDir))
        val serial = tail(File(stateDir, "guest-serial.log"), 40)
        return buildString {
            appendLine(summary?.verdict ?: "No crash report, so the app did not crash: iOS or the system killed it, " +
                "or it is still running.")
            if (guestLog != null) {
                appendLine()
                appendLine("——— Guest system log, last launch ———")
                appendLine(guestLog.ifBlank { "(the guest printed nothing)" })
            }
            if (summary != null) {
                appendLine()
                appendLine("——— Crash report ———")
                appendLine(summary.body)
            }
            if (engine.isNotEmpty()) {
                appendLine()
                appendLine("——— Engine log (emulation's own words) ———")
                engine.forEach { appendLine(it) }
            }
            if (serial.isNotEmpty()) {
                appendLine()
                appendLine("——— guest-serial.log ———")
                serial.forEach { appendLine(it) }
            }
        }.trimEnd()
    }
}
