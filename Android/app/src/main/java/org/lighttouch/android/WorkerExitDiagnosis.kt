package org.lighttouch.android

import android.app.ActivityManager
import android.content.Context
import java.io.File
import java.io.RandomAccessFile

/**
 * Why the :engine worker died, in one line the screen can show.
 *
 * The worker can die without a word: a native crash inside the engine or the shared library kills the process
 * before anything reports status, and the app is left with "exited unexpectedly". Two things outlive it — Android's
 * own record of the exit (its reason and description) and the engine log the worker redirected its output into,
 * which is where a C++ abort or a qemu error lands. Both are read here, nothing else.
 */
internal object WorkerExitDiagnosis {
    private const val TAIL_BYTES = 8 * 1024

    /** One line: Android's exit reason and description, then the last thing the engine said. */
    fun summarize(context: Context, files: File): String {
        val parts = mutableListOf<String>()
        system(context)?.let { parts += it }
        engineLogTail(files)?.let { parts += "engine: $it" }
        return parts.joinToString(" · ")
    }

    /** The newest own-worker exit receipt, as Android recorded it. */
    private fun system(context: Context): String? {
        val newest = File(context.filesDir, "worker-exits").listFiles()
            ?.filter { it.name.endsWith(".json") && !it.name.contains("unavailable") && !it.name.contains("capture-error") }
            ?.maxByOrNull { it.lastModified() } ?: return null
        return runCatching {
            val json = org.json.JSONObject(newest.readText())
            val reason = json.optInt("reason", -1)
            val description = json.optString("description")
            val name = when (reason) {
                ActivityManager.REASON_EXIT_SELF -> "exit_self"
                ActivityManager.REASON_SIGNALED -> "signaled"
                ActivityManager.REASON_LOW_MEMORY -> "low_memory"
                ActivityManager.REASON_CRASH -> "crash"
                ActivityManager.REASON_CRASH_NATIVE -> "native crash"
                ActivityManager.REASON_ANR -> "not responding"
                ActivityManager.REASON_INITIALIZATION_FAILURE -> "initialization failure"
                ActivityManager.REASON_PERMISSION_CHANGE -> "permission change"
                ActivityManager.REASON_EXCESSIVE_RESOURCE_USAGE -> "excessive resource usage"
                ActivityManager.REASON_USER_REQUESTED -> "user requested"
                ActivityManager.REASON_OTHER, -1 -> "reason $reason"
                else -> "reason $reason"
            }
            val status = json.optInt("status", 0)
            "android: $name" + (if (status != 0) " (status $status)" else "") +
                (if (description.isNotEmpty()) " $description" else "")
        }.getOrNull()
    }

    /**
     * The last thing the newest engine log says. A native crash inside the engine, or an exception escaping into
     * std::terminate on the shared library's side, prints there before the process goes.
     */
    private fun engineLogTail(files: File): String? {
        val newest = File(files, "library/records").listFiles()
            ?.mapNotNull { File(it, "state") }
            ?.flatMap { state -> state.listFiles { file -> file.name.startsWith("engine-") && file.name.endsWith(".log") }?.toList() ?: emptyList() }
            ?.maxByOrNull { it.lastModified() }
            ?: return null
        val text = runCatching {
            RandomAccessFile(newest, "r").use { file ->
                val length = file.length()
                val start = maxOf(0L, length - TAIL_BYTES)
                val buffer = ByteArray((length - start).toInt())
                file.seek(start)
                file.readFully(buffer)
                String(buffer, Charsets.UTF_8)
            }
        }.getOrNull() ?: return null
        // The last lines that say something: qemu's and the library's errors are one line each.
        return text.lines().map { it.trim() }.filter { it.isNotEmpty() }.takeLast(3)
            .joinToString(" / ").take(400).ifEmpty { null }
    }
}
