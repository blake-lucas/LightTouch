package org.lighttouch.android

import android.content.Context
import org.json.JSONObject
import java.io.File
import java.util.concurrent.TimeUnit

/** One app the guest has, as its installer lists it. */
data class InstalledApp(val bundleId: String, val name: String?, val build: String?, val shortVersion: String?) {
    /** What the app calls its version, which is what the store's version numbers match (the build number can be anything). */
    val version get() = shortVersion ?: build
    val title get() = name ?: bundleId
}

/**
 * Runs the bundled app-service helper (liblt_appservice.so, libimobiledevice over the
 * :engine worker's private usbmux socket) as a finite child process. One request per process:
 * a stuck library call can only hang that child, which the deadline kills.
 */
internal class GuestApps(private val context: Context) {
    class Failure(message: String) : Exception(message)

    /** Words that mark a line as part of the story: what dyld wanted, what killed the app, what failed. */
    private val INTERESTING = listOf("dyld", "libobjc", "SpringBoard", "backboardd", "launchd", "Jetsam", "watchdog",
        "EXC_", "Termination", "code signature", "sandbox", "assert", "abnormal", "not found", "no such file",
        "unable to", "failed", "error")

    private fun decodeHex(hex: String): ByteArray {
        val out = ByteArray(hex.length / 2)
        for (i in out.indices) {
            val high = hex[i * 2].digitToIntOrNull(16) ?: return out.copyOf(i)
            val low = hex[i * 2 + 1].digitToIntOrNull(16) ?: return out.copyOf(i)
            out[i] = ((high shl 4) or low).toByte()
        }
        return out
    }

    private val helper get() = File(context.applicationInfo.nativeLibraryDir, "liblt_appservice.so")
    val available get() = helper.canExecute()

    /** Streams each JSON event to [event]; returns the final "result" object or throws its "error". */
    fun run(socket: String, args: List<String>, timeoutSeconds: Long, cancelled: () -> Boolean = { false }, event: (JSONObject) -> Unit = {}): JSONObject {
        if (!available) throw Failure("Guest app services are not included in this build.")
        val process = ProcessBuilder(listOf(helper.absolutePath) + args.take(1) + listOf("--socket", socket) + args.drop(1))
            .redirectErrorStream(true).start()
        process.outputStream.close()
        val watchdog = Thread({
            try {
                // Polled, so a cancelled request stops within a quarter second.
                val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(timeoutSeconds)
                while (!process.waitFor(250, TimeUnit.MILLISECONDS)) {
                    if (cancelled() || System.nanoTime() > deadline) { process.destroyForcibly(); break }
                }
            } catch (_: InterruptedException) { }
        }, "guest-apps-watchdog").apply { isDaemon = true; start() }
        var result: JSONObject? = null; var error: String? = null; var lastLine = ""
        try {
            process.inputStream.bufferedReader().useLines { lines ->
                for (line in lines) {
                    if (line.isBlank()) continue
                    lastLine = line.take(300)
                    val value = try { JSONObject(line) } catch (_: org.json.JSONException) { continue }
                    value.optJSONObject("result")?.let { result = it }
                    if (value.has("error")) error = value.optString("error")
                    event(value)
                }
            }
            val code = process.waitFor()
            error?.let { throw Failure(it) }
            if (code != 0 || result == null) throw Failure(if (code == 137 || code == 9) "The guest did not answer in time; the result is unconfirmed."
                else "Guest app service exited ($code) without a result. ${lastLine}".trim())
            return result!!
        } finally { watchdog.interrupt(); process.destroy() }
    }

    fun target(socket: String): StoreTarget {
        var target: StoreTarget? = null
        run(socket, listOf("info"), 60) { e -> e.optJSONObject("target")?.let { target = StoreTarget.of(it.getString("product_type"), it.getString("ios")) } }
        return target ?: throw Failure("The guest did not report its device type.")
    }

    /** The apps the user added (not Apple's own), by name. */
    fun installedApps(socket: String): List<InstalledApp> {
        val apps = run(socket, listOf("list"), 90).optJSONArray("apps") ?: return emptyList()
        return (0 until apps.length()).mapNotNull { apps.optJSONObject(it) }.filter { it.optString("bundle_id").isNotEmpty() }
            .map { InstalledApp(it.optString("bundle_id"), it.text("name"), it.text("version"), it.text("short_version")) }
            .sortedBy { it.title.lowercase() }
    }

    /** The guest's newest crash report: (file name, the report's description text), or null if it has none. */
    fun lastCrash(socket: String): Pair<String, String>? {
        val result = run(socket, listOf("crashlog"), 60)
        val name = if (result.isNull("name")) return null else result.optString("name")
        val text = result.optString("text")
        // .plist reports wrap the human-readable report in a <string> under "description".
        val body = Regex("<key>description</key>\\s*<string>(.*?)</string>", RegexOption.DOT_MATCHES_ALL).find(text)?.groupValues?.get(1)
            ?.replace("&lt;", "<")?.replace("&gt;", ">")?.replace("&amp;", "&") ?: text
        return name to body
    }

    /**
     * Up to [seconds] of the guest's own system log, trimmed to the lines that explain a launch: dyld's
     * complaints, SpringBoard or the kernel killing an app, a missing service. This is the channel an app writes
     * to in the seconds before iOS writes its crash report, and the only one that shows a launch that never got
     * far enough to crash. Its chunks arrive hex-encoded with NUL separators, so a line may span two of them.
     */
    fun guestLog(socket: String, seconds: Int, cancelled: () -> Boolean = { false }, progress: (Int) -> Unit = {}): String {
        val text = StringBuilder()
        run(socket, listOf("syslog", "--seconds", seconds.toString()), seconds + 30L, cancelled) { event ->
            event.optJSONObject("syslog_chunk")?.optString("hex")?.takeIf { it.isNotEmpty() }?.let { hex ->
                text.append(String(decodeHex(hex), Charsets.UTF_8))
                // Bounded: an idle launch prints little, a busy one prints faster than anything reads it.
                if (text.length > 512 * 1024) text.delete(0, text.length - 256 * 1024)
                progress(text.length)
            }
        }
        val lines = text.toString().split('\u0000').flatMap { it.split('\n') }.map { it.trim() }.filter { it.isNotEmpty() }
        val explained = lines.filter { line -> INTERESTING.any { line.contains(it, ignoreCase = true) } }
        val chosen = if (explained.size > 60) explained.takeLast(60) else explained
        return if (chosen.isNotEmpty()) chosen.joinToString("\n") else lines.takeLast(40).joinToString("\n")
    }

    fun install(socket: String, ipa: File, bundleId: String, cancelled: () -> Boolean, event: (JSONObject) -> Unit): JSONObject =
        run(socket, listOf("install", "--ipa", ipa.absolutePath, "--bundle-id", bundleId), 900, cancelled, event)
}
