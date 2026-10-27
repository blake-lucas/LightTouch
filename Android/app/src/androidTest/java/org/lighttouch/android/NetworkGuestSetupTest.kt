package org.lighttouch.android

import android.content.Intent
import android.os.Bundle
import android.os.SystemClock
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** Opt-in real guest setup check. Run only after a clean guest shutdown: starting
 * instrumentation replaces the activity process. Leaves the guest for visual
 * Safari checks; the operator must shut it down cleanly afterwards. */
@RunWith(AndroidJUnit4::class)
class NetworkGuestSetupTest {
    @Test fun installsCertificateAndProxyAndOpensSafari() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val arguments = InstrumentationRegistry.getArguments()
        assumeTrue("Explicit real guest network check required", arguments.getString("exerciseNetworkGuest") == "true")
        val expectedMode = requireNotNull(arguments.getString("expectedNetworkMode")?.toIntOrNull())
        require(expectedMode in 1..2)
        val context = instrumentation.targetContext
        val evidence = File(context.filesDir, "network-setup-evidence-${System.currentTimeMillis()}").apply { check(mkdir()) }
        val receipt = JSONObject().put("setupPassed", false).put("safariPageVerified", false)
        @Suppress("DEPRECATION")
        fun encode(bundle: Bundle): JSONObject = JSONObject().apply {
            for (key in bundle.keySet()) when (val value = bundle.get(key)) {
                is Bundle -> put(key, encode(value))
                else -> put(key, value)
            }
        }
        try {
            ActivityScenario.launch(MainActivity::class.java).use {
                AcceptanceBinding(context, Intent(context, EngineService::class.java), IEngine.Stub::asInterface).use { binding ->
                    val worker = binding.get()
                    val initial = worker.status()
                    receipt.put("initial", encode(initial))
                    assertFalse("Run from a fresh stopped worker", initial.getBoolean("sessionStarted"))
                    assertEquals(expectedMode, NetworkConfiguration.load(context).nativeMode)
                    context.startForegroundService(Intent(context, EngineService::class.java))
                    val selected = requireNotNull(worker.records().firstOrNull { it.getBoolean("selected") }?.getString("id"))
                    val started = worker.start(selected)
                    assertEquals(started.toString(), "", started.getString("error"))
                    val deadline = SystemClock.elapsedRealtime() + 150_000
                    var status: Bundle
                    do {
                        status = worker.status()
                        receipt.put("latest", encode(status))
                        File(evidence, "result.json").writeText(receipt.toString(2))
                        if (status.getString("networkStatus").orEmpty().contains(" · Ready")) break
                        assertEquals(status.toString(), "", status.getString("error"))
                        Thread.sleep(500)
                    } while (SystemClock.elapsedRealtime() < deadline)
                    assertEquals(expectedMode, status.getInt("networkMode"))
                    assertTrue(status.toString(), status.getString("networkStatus").orEmpty().contains(" · Ready"))
                    assertTrue(status.getString("networkCertificateFingerprint").orEmpty().matches(Regex("[0-9A-F]{2}(:[0-9A-F]{2}){31}")))
                    val request = worker.guestRequest(2, "com.apple.mobilesafari", 15_000)
                    assertEquals(request.toString(), "", request.getString("error"))
                    val id = request.getLong("requestId")
                    val launchDeadline = SystemClock.elapsedRealtime() + 16_000
                    var reply: Bundle
                    do {
                        reply = worker.guestPoll(id)
                        if (reply.getInt("state") != 1) break
                        Thread.sleep(100)
                    } while (SystemClock.elapsedRealtime() < launchDeadline)
                    assertEquals(reply.toString(), 2, reply.getInt("state"))
                    receipt.put("safariLaunch", encode(reply)).put("setupPassed", true)
                }
            }
        } catch (error: Throwable) { receipt.put("failure", error.toString()); throw error }
        finally { File(evidence, "result.json").writeText(receipt.toString(2)) }
    }
}
