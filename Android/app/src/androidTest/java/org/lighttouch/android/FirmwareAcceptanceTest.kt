package org.lighttouch.android

import android.content.Intent
import android.os.*
import androidx.test.core.app.ActivityScenario
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

/**
 * Expensive real preparation is never part of the default test suite. Stage the stock IPSW at
 * files/acceptance-fw/stock.ipsw, then run with -e preparePrivateFirmware true [-e firmwareId n81ap-10B500].
 */
@RunWith(AndroidJUnit4::class)
class FirmwareAcceptanceTest {
    @Test fun cancelMidwayThenPrepareAndImportInFreshWorker() {
        assumeTrue("Explicit firmware opt-in required", InstrumentationRegistry.getArguments().getString("preparePrivateFirmware") == "true")
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val stock = File(context.filesDir, "acceptance-fw/stock.ipsw")
        assertTrue(stock.isFile)
        val stockSha = acceptanceSha256(stock)
        val tools = GuestTools.firmwareTools(context)
        val evidence = File(context.filesDir, "firmware-evidence-${System.currentTimeMillis()}").apply { check(mkdir()) }
        val result = JSONObject().put("pass", false)
        val trace = JSONArray()
        val id = InstrumentationRegistry.getArguments().getString("firmwareId", "n72ap-7E18")
        val firmware = FirmwareCatalog.load(context).single { it.id == id }
        val timeoutMs = InstrumentationRegistry.getArguments().getString("firmwareTimeoutSeconds", "1800").toLong().coerceIn(60, 7200) * 1000
        try {
            ActivityScenario.launch(MainActivity::class.java).use { scenario ->
                var previousPid = 0
                for (cancelMidway in listOf(true, false)) {
                    AcceptanceBinding(context, Intent(context, FirmwareService::class.java), IFirmware.Stub::asInterface).use { binding ->
                        val worker = binding.get()
                        val initial = worker.status()
                        assertEquals("Fresh preparer required", "idle", initial.getString("state"))
                        assertEquals(Process.myUid(), initial.getInt("uid"))
                        assertNotEquals(Process.myPid(), initial.getInt("pid"))
                        if (previousPid != 0) assertNotEquals("Retry reused consumed process", previousPid, initial.getInt("pid"))
                        previousPid = initial.getInt("pid")
                        assertTrue("firmwarekit ships in the APK", worker.supports(id))
                        scenario.onActivity { it.startForegroundService(Intent(it, FirmwareService::class.java)) }
                        val started = SystemClock.elapsedRealtime()
                        val job = worker.start(id, stock.absolutePath, tools.absolutePath, "", false)
                        val destination = File(requireNotNull(job.getString("destination")))
                        var terminal = job
                        var lastStage = ""
                        while (terminal.getString("state") in listOf("starting", "preparing", "cancelling")) {
                            terminal = worker.status()
                            val stage = terminal.getString("stage") ?: ""
                            if (stage != lastStage) {
                                trace.put(JSONObject().put("pid", previousPid).put("stage", stage).put("done", terminal.getLong("done"))
                                    .put("total", terminal.getLong("total")).put("elapsedMs", SystemClock.elapsedRealtime() - started))
                                File(evidence, "progress.json").writeText(trace.toString(2)); lastStage = stage
                            }
                            // A third of the way in, firmwarekit is past decrypting and is writing the volumes.
                            if (cancelMidway && terminal.getString("state") == "preparing" && terminal.getLong("total") > 0 &&
                                terminal.getLong("done") * 3 > terminal.getLong("total")) worker.cancel()
                            if (SystemClock.elapsedRealtime() - started > timeoutMs) { worker.cancel(); error("Preparation timed out; see cache/firmwarekit.log") }
                            Thread.sleep(100)
                        }
                        File(evidence, if (cancelMidway) "cancel-result.txt" else "complete-result.txt").writeText(terminal.toString())
                        result.put(if (cancelMidway) "cancelElapsedMs" else "prepareElapsedMs", SystemClock.elapsedRealtime() - started)
                        if (cancelMidway) {
                            assertEquals(terminal.toString(), "cancelled", terminal.getString("state"))
                            assertFalse("Cancelled output admitted", destination.exists())
                            assertTrue("Cancelled work leaked", destination.parentFile!!.listFiles()!!.none { it.name.startsWith("work-") })
                        } else {
                            assertEquals(terminal.toString(), "complete", terminal.getString("state")); assertEquals(0, terminal.getInt("code"))
                            assertTrue(destination.isDirectory)
                            AcceptanceBinding(context, Intent(context, EngineService::class.java), IEngine.Stub::asInterface).use { engineBinding ->
                                val engine = engineBinding.get()
                                assertEquals("Import must not interrupt a guest", "stopped", engine.status().getString("state"))
                                val before = engine.records().map { it.getString("id") }.toSet()
                                val importStart = SystemClock.elapsedRealtime()
                                assertEquals("Prepared base admission", "", engine.adoptPrepared(destination.absolutePath, "$id Android prepared", File(context.filesDir, "bootrom.bin").absolutePath))
                                val after = engine.records()
                                assertTrue("Existing devices lost", after.map { it.getString("id") }.containsAll(before))
                                val record = after.single { it.getString("id") !in before }
                                assertEquals(firmware.board, record.getString("board")); assertEquals(firmware.build, record.getString("build"))
                                assertTrue("Guest package must be available", record.getBoolean("guestPackageAvailable"))
                                result.put("recordId", record.getString("id")).put("importElapsedMs", SystemClock.elapsedRealtime() - importStart)
                            }
                        }
                        assertEquals("The IPSW is only read", stockSha, acceptanceSha256(stock))
                        val died = CountDownLatch(1)
                        val binder = worker.asBinder()
                        val recipient = IBinder.DeathRecipient { died.countDown() }
                        binder.linkToDeath(recipient, 0)
                        worker.finish()
                        assertTrue("Consumed firmware process did not exit", died.await(10, TimeUnit.SECONDS))
                        try { binder.unlinkToDeath(recipient, 0) } catch (_: java.util.NoSuchElementException) { }
                    }
                }
                result.put("pass", true).put("appUid", Process.myUid()).put("freshRetryPid", previousPid)
            }
        } catch (failure: Throwable) { result.put("failure", failure.toString()); throw failure }
        finally { File(evidence, "result.json").writeText(result.toString(2)) }
    }
}
