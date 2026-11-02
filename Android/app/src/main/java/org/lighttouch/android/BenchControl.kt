package org.lighttouch.android

import android.content.Intent
import android.os.SystemClock
import android.util.Log
import java.io.File

/**
 * Benchmark control over ADB, in debuggable builds only (tools/bench drives it): the same verbs the desktop's
 * `lighttouch run --control` takes, so one harness runs a test matrix on both.
 *
 *   adb shell am start --activity-single-top -n org.lighttouch.android/.MainActivity \
 *       -a org.lighttouch.android.BENCH --es cmd CMD [--es arg ARG]
 *
 * CMD: select RECORD | start | stop | status | button "N down|up|press" | tap "X Y" | swipe "X0 Y0 X1 Y1 MS" |
 * launch BUNDLE_ID | frontmost | lockstatus | frames "start NAME" | frames stop. Coordinates are the guest screen's, normalized
 * (0..1, y down). Every command answers with one logcat line, tag LTBench: "CMD -> RESULT". `frames start NAME` keeps
 * appending the engine's frame timeline (`sequence published_ns presented_ns`, CLOCK_MONOTONIC) to
 * files/bench/NAME.frames until `frames stop`.
 */
class BenchControl(private val host: Host, private val dir: File) {
    interface Host {
        val engine: IEngine?
        val status: String
        val displayAsleep: Boolean
        fun select(id: String)
        fun start()
        fun stop()
    }

    @Volatile private var recording: Thread? = null

    fun handle(intent: Intent) {
        val cmd = intent.getStringExtra("cmd") ?: return
        val arg = intent.getStringExtra("arg") ?: ""
        // Off the main thread: guest requests and gestures wait.
        Thread({ answer("$cmd $arg".trim(), try { run(cmd, arg) } catch (e: Exception) { "error: ${e.message}" }) }, "bench-$cmd").start()
    }

    private fun answer(command: String, result: String) {
        Log.i(TAG, "$command -> $result")
    }

    private fun engine(): IEngine = host.engine ?: error("no engine worker")

    private fun run(cmd: String, arg: String): String = when (cmd) {
        "select" -> { onMain { host.select(arg) }; "ok" }
        "start" -> { onMain { host.start() }; "ok" }
        "stop" -> { onMain { host.stop() }; "ok" }
        "status" -> "state=${host.status} asleep=${host.displayAsleep}"
        "button" -> {
            val (button, how) = arg.split(" ").let { it[0].toInt() to it.getOrElse(1) { "press" } }
            if (how != "up") engine().button(button, true)
            if (how == "press") Thread.sleep(100)
            if (how != "down") engine().button(button, false)
            "ok"
        }
        "tap" -> {
            val (x, y) = arg.split(" ").map { it.toFloat() }
            touch(0, x, y); Thread.sleep(150); touch(2, x, y)
        }
        "swipe" -> {
            val v = arg.split(" ").map { it.toFloat() }
            val steps = 15
            var result = touch(0, v[0], v[1])
            for (i in 1..steps) {
                Thread.sleep((v[4] / steps).toLong())
                val f = i.toFloat() / steps
                result = touch(1, v[0] + (v[2] - v[0]) * f, v[1] + (v[3] - v[1]) * f)
            }
            touch(2, v[2], v[3]).ifEmpty { result }
        }
        "launch" -> guest(LAUNCH_APP, arg, 15000)
        "frontmost" -> guest(FRONTMOST, "", 5000)
        "lockstatus" -> guest(LOCK_STATUS, "", 5000)
        "frames" -> frames(arg)
        else -> "error: unknown command"
    }.ifEmpty { "ok" }

    private fun touch(phase: Int, x: Float, y: Float): String =
        // width -1: the coordinates are already normalized guest ones
        engine().touch(SystemClock.uptimeMillis() * 1_000_000, intArrayOf(1), intArrayOf(phase), floatArrayOf(x, y), -1, -1, 0)

    private fun guest(operation: Int, argument: String, timeoutMs: Int): String {
        val worker = engine()
        val request = worker.guestRequest(operation, argument, timeoutMs)
        request.getString("error")?.takeIf { it.isNotEmpty() }?.let { return "error: $it" }
        val id = request.getLong("requestId")
        val deadline = SystemClock.uptimeMillis() + timeoutMs + 2000
        while (SystemClock.uptimeMillis() < deadline) {
            val poll = worker.guestPoll(id)
            val state = poll.getInt("state")
            if (state >= 2) return "state=$state status=${poll.getInt("guestStatus")} output=${poll.getString("output")?.trim()}"
            Thread.sleep(100)
        }
        worker.guestCancel(id)
        return "error: no answer"
    }

    private fun frames(arg: String): String {
        val parts = arg.split(" ")
        recording?.interrupt(); recording?.join(2000); recording = null
        if (parts[0] != "start") return "stopped"
        val name = parts.getOrNull(1)?.takeIf { it.matches(Regex("[A-Za-z0-9._-]+")) } ?: return "error: frames start NAME"
        dir.mkdirs()
        val file = File(dir, "$name.frames")
        val worker = engine()
        recording = Thread({
            var since = Long.MAX_VALUE
            file.bufferedWriter().use { out ->
                try {
                    while (!Thread.currentThread().isInterrupted) {
                        val stats = worker.frameStats(if (since == Long.MAX_VALUE) 0 else since)
                        val frames = stats.getLongArray("frames") ?: LongArray(0)
                        // quads: sequence, published, presented, presenter work; the first poll only sets the start
                        var i = 0
                        while (i + 3 < frames.size) {
                            if (since != Long.MAX_VALUE) out.write("${frames[i]} ${frames[i + 1]} ${frames[i + 2]}\n")
                            i += 4
                        }
                        if (frames.size >= 4) since = frames[frames.size - 4]
                        else if (since == Long.MAX_VALUE) since = 0
                        out.flush()
                        Thread.sleep(250)
                    }
                } catch (_: InterruptedException) {
                } catch (e: Exception) { answer("frames", "error: ${e.message}") }
            }
        }, "bench-frames").apply { start() }
        return "recording ${file.name}"
    }

    private fun onMain(block: () -> Unit) {
        val done = java.util.concurrent.CountDownLatch(1)
        android.os.Handler(android.os.Looper.getMainLooper()).post { try { block() } finally { done.countDown() } }
        done.await()
    }

    companion object {
        const val ACTION = "org.lighttouch.android.BENCH"
        const val TAG = "LTBench"
        private const val FRONTMOST = 1
        private const val LAUNCH_APP = 11
        private const val LOCK_STATUS = 12
    }
}
