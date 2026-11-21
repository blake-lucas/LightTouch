package org.lighttouch.android

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.os.IBinder
import android.os.Process
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** Existing focused Notes UI only. Captures require independent full-body visual review. */
@RunWith(AndroidJUnit4::class)
class GuestNotesPersistenceTest {
    @Test fun collectSingleWriteOrReadOnlyNotesEvidence() {
        val args = InstrumentationRegistry.getArguments()
        assumeTrue("Explicit Notes persistence opt-in required", args.getString("exerciseNotesPersistence") == "true")
        val mode = requireNotNull(args.getString("notesMode")).also { require(it in listOf("write", "read")) }
        require(args.getString("existingNotesSessionConfirmed") == "true")
        if (mode == "write") require(args.getString("focusedBlankNotesEditorConfirmed") == "true")
        else require(args.getString("notesBodyVisibleConfirmed") == "true")
        val marker = requireNotNull(args.getString("notesMarker")).also {
            require(it.matches(Regex("LTNOTES-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")))
        }
        val expectedApk = requireNotNull(args.getString("expectedMainApkSha256")).also { require(it.matches(Regex("[0-9a-f]{64}"))) }
        val pid = requireNotNull(args.getString("expectedWorkerPid")?.toIntOrNull()).also { require(it > 0) }
        val ticks = requireNotNull(args.getString("expectedWorkerStartTicks")).also { require((it.toLongOrNull() ?: 0) > 0) }
        val boot = requireNotNull(args.getString("expectedBootId")).also { require(it.matches(Regex("[0-9a-f-]{36}"))) }
        val record = requireNotNull(args.getString("recordId")).also { require(it.matches(Regex("[0-9a-f]{32}"))) }
        val lockHash = requireNotNull(args.getString("expectedBaseLockSha256")).also { require(it.matches(Regex("[0-9a-f]{64}"))) }
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val evidence = File(context.filesDir, "notes-evidence-${mode}-${System.currentTimeMillis()}").apply { check(mkdir()) }
        val result = JSONObject().put("schema", 1).put("guestAcceptance", false).put("guestAcceptancePassed", false)
            .put("collectionComplete", false).put("mode", mode).put("marker", marker)
            .put("guestUiTextNotPhysicalKeyboardProof", true).put("persistenceVerified", false)
            .put("rootVisualFullBodyReviewRequired", true).put("startsResumesRecoversStopsGuest", false)
            .put("mainApkSha256", expectedApk).put("workerPid", pid).put("workerStartTicks", ticks)
            .put("bootId", boot).put("recordId", record).put("baseLockSha256", lockHash)
            .put("clock", "Android SystemClock.elapsedRealtime/BOOTTIME").put("requestTimeoutMs", 10_000)
        val stages = JSONArray(); val operations = JSONArray()
        result.put("stages", stages).put("operations", operations)
        @Suppress("DEPRECATION")
        fun encode(b: Bundle): JSONObject = JSONObject().apply {
            for (key in b.keySet()) when (val v = b.get(key)) {
                is Bundle -> put(key, encode(v))
                is IntArray -> put(key, JSONArray(v.toList()))
                is LongArray -> put(key, JSONArray(v.toList()))
                else -> put(key, v ?: JSONObject.NULL)
            }
        }
        fun persist() {
            val pending = File(evidence, ".result.pending")
            pending.writeText(result.toString(2)); check(pending.renameTo(File(evidence, "result.json")))
        }
        fun stage(label: String) { stages.put(JSONObject().put("stage", label).put("elapsedMs", SystemClock.elapsedRealtime())); persist() }
        var bound = false
        val connected = CountDownLatch(1)
        var engine: IEngine? = null
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName, binder: IBinder) { engine = IEngine.Stub.asInterface(binder); connected.countDown() }
            override fun onServiceDisconnected(name: ComponentName) = Unit
        }
        try {
            stage("identity-preflight")
            assertTrue(context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0)
            assertNotEquals(Process.myPid(), pid)
            assertEquals(expectedApk, acceptanceSha256(File(context.applicationInfo.sourceDir)))
            val stateDir = File(context.filesDir, "library/records/$record/state").canonicalFile
            val lock = File(context.filesDir, "library/records/$record/base/device.lock.json")
            fun processIdentity() {
                assertEquals(boot, File("/proc/sys/kernel/random/boot_id").readText().trim())
                val fields = File("/proc/$pid/stat").readText().substringAfterLast(')').trim().split(Regex("\\s+"))
                assertEquals("Worker PID reused", ticks, fields[19])
                val uid = File("/proc/$pid/status").useLines { lines -> lines.first { it.startsWith("Uid:") } }
                    .substringAfter(':').trim().split(Regex("\\s+"))[0].toInt()
                assertEquals(Process.myUid(), uid)
            }
            processIdentity()
            // AcceptanceBinding uses AUTO_CREATE. Flags0 intentionally bind only an existing service.
            bound = context.bindService(Intent(context, EngineService::class.java), connection, 0)
            assertTrue("Existing service unavailable; never create a replacement", bound)
            assertTrue("Existing Binder connection timeout", connected.await(5, TimeUnit.SECONDS))
            val worker = requireNotNull(engine)
            val overallDeadline = SystemClock.elapsedRealtime() + 60_000
            result.put("overallClientDeadlineElapsedMs", overallDeadline)
            fun status(label: String): Bundle {
                processIdentity()
                check(SystemClock.elapsedRealtime() < overallDeadline) { "Client collection deadline exceeded" }
                val s = worker.status()
                stages.put(JSONObject().put("stage", label).put("elapsedMs", SystemClock.elapsedRealtime()).put("status", encode(s))); persist()
                assertEquals(pid, s.getInt("pid")); assertEquals(Process.myUid(), s.getInt("uid"))
                assertTrue("Consumed session required", s.getBoolean("sessionStarted")); assertEquals("running", s.getString("state"))
                assertEquals("Existing worker error", "", s.getString("error"))
                assertTrue("Actual consumed engine log (docs/LOGS.md)", File(requireNotNull(s.getString("engineLog"))).canonicalPath in
                    listOf("engine.log", "engine.tail.log").map { File(context.filesDir, "logs/$it").canonicalPath })
                val selected = worker.records().filter { it.getBoolean("selected") }
                assertEquals("Exactly one selected record", 1, selected.size)
                assertEquals(record, selected.single().getString("id"))
                assertEquals("Immutable base lock changed", lockHash, acceptanceSha256(lock))
                return s
            }
            fun capture(label: String) {
                status("$label-before-capture")
                val target = File(context.filesDir, "screenshots/${evidence.name}/$label.png")
                val error = worker.screenshot(target.absolutePath)
                result.put(label, JSONObject().put("path", target.absolutePath).put("elapsedMs", SystemClock.elapsedRealtime()).put("error", error)); persist()
                assertEquals("Actual capture failed", "", error)
                result.getJSONObject(label).put("sha256", acceptanceSha256(target)); persist()
                status("$label-after-capture")
            }
            fun operation(op: Int, name: String, argument: String): String {
                status("$name-preflight")
                val start = SystemClock.elapsedRealtime()
                val deadline = minOf(overallDeadline, start + 11_000)
                val item = JSONObject().put("operation", name).put("code", op).put("argument", argument)
                    .put("startedElapsedMs", start).put("clientDeadlineElapsedMs", deadline)
                operations.put(item); persist()
                val queued = worker.guestRequest(op, argument, 10_000)
                val id = queued.getLong("requestId")
                item.put("admission", encode(queued)).put("requestId", id); persist()
                var primary: Throwable? = null
                try {
                    assertEquals("Guest request admission", "", queued.getString("error")); assertTrue("Owned request ID required", id > 0)
                    val polls = JSONArray(); item.put("polls", polls)
                    while (SystemClock.elapsedRealtime() < deadline) {
                        processIdentity()
                        val reply = worker.guestPoll(id)
                        polls.put(JSONObject().put("elapsedMs", SystemClock.elapsedRealtime()).put("reply", encode(reply)))
                        // Small bounded reply log; full Native.status is only sampled at stage boundaries.
                        persist()
                        assertEquals("Guest poll error", "", reply.getString("error"))
                        when (reply.getInt("state")) {
                            2 -> {
                                assertEquals(reply.toString(), 0, reply.getInt("guestStatus"))
                                item.put("completedElapsedMs", SystemClock.elapsedRealtime()); persist()
                                check(SystemClock.elapsedRealtime() < deadline) { "Reply arrived after client deadline" }
                                return reply.getString("output") ?: ""
                            }
                            3, 4, 5 -> error("Guest operation terminal failure: $reply")
                        }
                        Thread.sleep(minOf(250L, (deadline - SystemClock.elapsedRealtime()).coerceAtLeast(1)))
                    }
                    error("Guest operation timed out; never retry a possible mutation")
                } catch (failure: Throwable) { primary = failure; item.put("failure", failure.toString()); throw failure }
                finally {
                    if (id > 0) try {
                        worker.guestCancel(id); item.put("cancelRequestedElapsedMs", SystemClock.elapsedRealtime())
                    } catch (cleanup: Throwable) {
                        item.put("cancelError", cleanup.toString())
                        if (primary != null) primary.addSuppressed(cleanup) else throw cleanup
                    } finally { persist() }
                }
            }
            capture("before")
            val frontmost = operation(1, "FRONTMOST-before", "")
            result.put("frontmostBefore", frontmost); persist()
            assertEquals("Actual Notes frontmost required", "com.apple.mobilenotes", frontmost.lineSequence().firstOrNull())
            if (mode == "write") {
                // Reserve before admission: even an unknown outcome must not be retried.
                val reservation = File(context.filesDir, "notes-write-once-$marker.json")
                check(reservation.createNewFile()) { "Marker already reserved; never retry a possible insertion" }
                reservation.writeText(JSONObject().put("evidence", evidence.name).put("record", record).put("pid", pid).toString())
                result.put("writeOnceReservation", reservation.absolutePath)
                stage("single-type-text-no-retry")
                operation(3, "TYPE_TEXT", marker)
                operation(4, "SYNC", "")
            }
            val after = operation(1, "FRONTMOST-after", "")
            result.put("frontmostAfter", after); persist()
            assertEquals("Notes must remain frontmost", "com.apple.mobilenotes", after.lineSequence().firstOrNull())
            capture("after")
            result.put("collectionComplete", true)
            stage("captured-awaiting-root-full-body-review-and-separate-stop-restart")
        } catch (failure: Throwable) {
            result.put("failure", failure.toString())
            // Diagnostic failure must not erase the original error or trigger another mutation.
            try {
                engine?.let { worker ->
                    val s = worker.status(); result.put("failureStatus", encode(s))
                    val fields = File("/proc/$pid/stat").readText().substringAfterLast(')').trim().split(Regex("\\s+"))
                    check(fields[19] == ticks && s.getInt("pid") == pid && s.getInt("uid") == Process.myUid())
                    val target = File(context.filesDir, "screenshots/${evidence.name}/failure.png")
                    result.put("failureCapture", JSONObject().put("path", target.absolutePath)
                        .put("error", worker.screenshot(target.absolutePath)).put("elapsedMs", SystemClock.elapsedRealtime()))
                }
            } catch (diagnostic: Throwable) { result.put("failureCaptureError", diagnostic.toString()) }
            throw failure
        } finally {
            try { if (bound) context.unbindService(connection) }
            finally { persist() }
        }
    }
}
