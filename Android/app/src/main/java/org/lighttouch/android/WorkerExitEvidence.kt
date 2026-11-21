package org.lighttouch.android

import android.app.ActivityManager
import android.content.Context
import android.os.Build
import android.os.SystemClock
import org.json.JSONObject
import java.io.File

/** Own :engine exit history only; never reads another app or system log stream. */
internal object WorkerExitEvidence {
    private const val TRACE_LIMIT = 1024 * 1024
    fun capture(context: Context, workerPid: Int) {
        if (Build.VERSION.SDK_INT < 30) return
        val directory = File(context.filesDir, "worker-exits").apply { mkdirs() }
        // A receipt (and possibly a trace) per worker exit, never removed before (536 on one phone).
        directory.listFiles()?.sortedByDescending { it.lastModified() }?.drop(10)?.forEach { it.delete() }
        val manager = context.getSystemService(ActivityManager::class.java)
        val deadline = SystemClock.elapsedRealtime() + 10_000
        try {
            while (true) {
                val record = manager.getHistoricalProcessExitReasons(context.packageName, workerPid.coerceAtLeast(0), 8)
                    .firstOrNull { it.processName == "${context.packageName}:engine" && (workerPid == 0 || it.pid == workerPid) }
                if (record != null) {
                    val prefix = "engine-${record.pid}-${record.timestamp}"
                    val receipt = File(directory, "$prefix.json")
                    if (receipt.exists()) return
                    val metadata = JSONObject().put("pid", record.pid).put("process", record.processName)
                        .put("timestampMs", record.timestamp).put("reason", record.reason).put("status", record.status)
                        .put("description", record.description).put("importance", record.importance)
                        .put("pssKiB", record.pss).put("rssKiB", record.rss).put("traceLimitBytes", TRACE_LIMIT)
                    try {
                        record.traceInputStream?.use { input ->
                            val target = File(directory, "$prefix.trace")
                            var written = 0
                            target.outputStream().use { output ->
                                val buffer = ByteArray(16 * 1024)
                                while (written < TRACE_LIMIT) {
                                    val count = input.read(buffer, 0, minOf(buffer.size, TRACE_LIMIT - written))
                                    if (count < 0) break
                                    output.write(buffer, 0, count); written += count
                                }
                            }
                            metadata.put("tracePath", target.absolutePath).put("traceBytes", written)
                                .put("traceTruncated", written == TRACE_LIMIT && input.read() >= 0)
                        } ?: metadata.put("traceUnavailable", true)
                    } catch (failure: Exception) { metadata.put("traceError", failure.toString()) }
                    receipt.writeText(metadata.toString(2)); return
                }
                if (SystemClock.elapsedRealtime() >= deadline) {
                    File(directory, "engine-$workerPid-unavailable.json").writeText(JSONObject().put("pid", workerPid)
                        .put("ownEngineExitHistoryUnavailable", true).toString(2)); return
                }
                Thread.sleep(500)
            }
        } catch (failure: Exception) {
            File(directory, "engine-$workerPid-capture-error.json").writeText(JSONObject().put("pid", workerPid).put("error", failure.toString()).toString(2))
        }
    }
}
