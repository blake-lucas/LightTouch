package org.lighttouch.android

import android.os.Bundle
import android.os.SystemClock
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.RandomAccessFile

/** Bounded reads of this worker's private trace; guest virtual time is not host time. */
internal class GuestTouchWireEvidence(filesDir: File, status: Bundle, private val evidence: File) {
    private val log = File(requireNotNull(status.getString("engineLog"))).canonicalFile
    private val raw = File(evidence, "touch-wire.log")
    private val lines = JSONArray()
    private var offset: Long
    private val startOffset: Long
    private var retained = 0
    private var partial = ""
    private var limitReached = false
    private var collectedFrames = 0
    private var serializedContacts = 0
    private val matcher = Regex("\\[MT ([0-9]+\\.[0-9]+)\\] (.*)")

    init {
        require(status.getBoolean("touchTraceConfigured")) { "Fixed trace must be configured before guest start in this worker" }
        require(log.toPath().startsWith(filesDir.canonicalFile.toPath()) && log.name in listOf("engine.log", "engine.tail.log") && log.isFile)
        offset = log.length(); startOffset = offset
        raw.writeBytes(byteArrayOf())
    }

    fun drain(touchIndex: Int) {
        RandomAccessFile(log, "r").use { input ->
            require(input.length() >= offset) { "Owned engine log was truncated" }
            input.seek(offset)
            val bytes = ByteArray(8192)
            while (retained < 4 * 1024 * 1024) {
                val count = input.read(bytes, 0, minOf(bytes.size, 4 * 1024 * 1024 - retained))
                if (count < 0) break
                val chunkOffset = offset
                raw.appendBytes(bytes.copyOf(count)); offset += count; retained += count
                val pieces = (partial + String(bytes, 0, count, Charsets.UTF_8)).split('\n')
                partial = pieces.last().takeLast(8192)
                for (line in pieces.dropLast(1)) {
                    val match = matcher.find(line) ?: continue
                    val message = match.groupValues[2]
                    if (!(message.startsWith("FRAME_READ") || message.startsWith("FINGER ") || message.startsWith("frame "))) continue
                    if (message.startsWith("FRAME_READ pending=yes")) collectedFrames++
                    if (Regex("^frame [0-9]+ len [0-9]+ nf [1-8] slot").containsMatchIn(message)) serializedContacts++
                    if (lines.length() < 8192) lines.put(JSONObject().put("line", line).put("guestVirtualSeconds", match.groupValues[1])
                        .put("observedAfterTouchIndex", touchIndex).put("hostReadUptimeMs", SystemClock.uptimeMillis()).put("readChunkOffset", chunkOffset))
                    else limitReached = true
                }
            }
            if (input.length() > offset) limitReached = true
        }
        persist()
    }

    fun collectedFrameCount(): Int = collectedFrames
    fun serializedContactCount(): Int = serializedContacts
    private fun persist() {
        File(evidence, "touch-wire.json").writeText(JSONObject().put("engineLog", log.absolutePath).put("startOffset", startOffset)
            .put("nextOffset", offset).put("retainedBytes", retained).put("limitBytes", 4 * 1024 * 1024).put("limitReached", limitReached)
            .put("frameReadPendingYesCount", collectedFrames).put("serializedContactRecords", serializedContacts).put("records", lines).put("partialTrailingLine", partial)
            .put("clockRelationship", "host reads and guest virtual timestamps have distinct epochs; read intervals do not prove per-batch collection")
            .put("guestAcceptancePassed", false).toString(2))
    }
}
