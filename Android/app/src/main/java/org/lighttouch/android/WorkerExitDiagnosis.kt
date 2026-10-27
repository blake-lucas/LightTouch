package org.lighttouch.android

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
        val (receipt, tombstone) = system(context)
        // Most specific first, in case the screen cuts the line: the crash's frames, then the engine's last
        // words, then how Android saw the exit.
        tombstone?.let { parts += it }
        engineLogTail(files)?.let { parts += it }
        receipt?.let { parts += it }
        return parts.joinToString(" · ")
    }

    /** The newest own-worker exit receipt, as Android recorded it, and its tombstone when it has one. */
    private fun system(context: Context): Pair<String?, String?> {
        val newest = File(context.filesDir, "worker-exits").listFiles()
            ?.filter { it.name.endsWith(".json") && !it.name.contains("unavailable") && !it.name.contains("capture-error") }
            ?.maxByOrNull { it.lastModified() } ?: return null to null
        return runCatching {
            val json = org.json.JSONObject(newest.readText())
            val tombstone = json.optString("tracePath").takeIf { it.isNotEmpty() }?.let { signalAndFrames(File(it)) }
            val reason = json.optInt("reason", -1)
            val description = json.optString("description")
            // ApplicationExitInfo's values; spelled out so nothing here needs API 30 at run time.
            val name = when (reason) {
                1 -> "exit_self"
                2 -> "signaled"
                3 -> "low_memory"
                4 -> "crash"
                5 -> "native crash"
                6 -> "not responding"
                7 -> "initialization failure"
                8 -> "permission change"
                9 -> "excessive resource usage"
                10 -> "user requested"
                11 -> "user stopped"
                12 -> "dependency died"
                13 -> "other"
                else -> "reason $reason"
            }
            val status = json.optInt("status", 0)
            ("android: $name" + (if (status != 0) " (status $status)" else "") +
                (if (description.isNotEmpty()) " $description" else "")) to tombstone
        }.getOrNull() ?: (null to null)
    }

    /**
     * A native crash's tombstone: the abort message and the top of the backtrace, where the library and
     * function that faulted are named.
     */
    private fun signalAndFrames(file: File): String? {
        if (!file.isFile || file.length() == 0L) return null
        val lines = runCatching { file.useLines { it.take(400).toList() } }.getOrNull() ?: return null
        val interesting = lines.firstOrNull { it.contains("signal ") || it.contains("Aborted") }
            ?.trim()?.take(120)
        val frames = lines.filter { it.trimStart().startsWith("#") }.take(3).map { it.trim().take(120) }
        val text = (listOfNotNull(interesting) + frames).joinToString(" / ")
        return text.ifEmpty { null }
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
