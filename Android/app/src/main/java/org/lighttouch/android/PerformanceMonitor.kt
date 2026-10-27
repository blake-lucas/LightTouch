package org.lighttouch.android

import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.RemoteException
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** One overlay refresh. Times in ms; null where the engine could not say. */
data class PerfSnapshot(
    val fps: Float = 0f, val frameMs: Float = 0f, val lowFps: Float = 0f, val maxFrameMs: Float = 0f,
    val presentMs: Float? = null, val latencyMs: Float? = null,
    val vcpuPercent: Float? = null, val vcpuCore: Int? = null, val vcpuMHz: Int? = null, val workerPercent: Float? = null,
    val pssMiB: Float? = null,
    val storage: StorageRates? = null,
    val glMsPerFrame: Float? = null, val bqlMsPerFrame: Float? = null, val runPercent: Float? = null,
    /** Frame intervals (ms) of the last few seconds, oldest first, for the graph. */
    val intervals: FloatArray = FloatArray(0),
    val detailedAvailable: Boolean = false,
    val detailedError: String? = null,
    /** No frame for over a second: the guest screen is static, which is not a slowdown. */
    val idle: Boolean = false,
)

/** Guest storage per second over the last second, from the engine's NAND counters. */
data class StorageRates(
    val readBytes: Double, val readOps: Double, val writeBytes: Double, val writeOps: Double, val eraseOps: Double,
    /** Host time spent serving NAND operations, percent of wall time. */
    val busyPercent: Double,
    /** Host time per operation, µs; 0 while idle. */
    val usPerOp: Double,
)

/** A gap this long between guest frames is a static screen (iOS publishes only on change), not a frame. */
internal const val IDLE_GAP_MS = 1000f

/**
 * Polls the engine's frame timeline and CPU counters four times a second while the overlay is
 * shown. Binder calls run on a private thread; snapshots are published on the main thread.
 */
internal class PerformanceMonitor(private val engine: () -> IEngine?) {
    var snapshot by mutableStateOf(PerfSnapshot())
        private set
    private val main = Handler(Looper.getMainLooper())
    private val thread = HandlerThread("performance-monitor").apply { start() }
    private val worker = Handler(thread.looper)
    @Volatile private var active = false
    @Volatile private var wantMemory = false
    // Worker-thread state.
    private var since = 0L
    private var binder: Any? = null
    private val published = LongArray(1200)  // ring of publish times, ns
    private val presented = LongArray(1200)
    private val work = LongArray(1200)
    private var count = 0
    private var lastCpu: LongArray? = null
    private var lastCpuAt = 0L
    private var lastMemoryAt = 0L
    private var pss: Float? = null
    private var vcpu: Float? = null
    private var workerCpu: Float? = null
    // Storage counter readings over the last second, oldest first: (nowNs, counters).
    private val storageSamples = ArrayDeque<Pair<Long, LongArray>>()
    private var storage: StorageRates? = null

    fun setActive(on: Boolean, memory: Boolean) {
        wantMemory = memory
        if (on == active) return
        active = on
        if (on) worker.post(tick) else worker.removeCallbacks(tick)
    }

    private val tick = object : Runnable {
        override fun run() {
            if (!active) return
            poll()
            worker.postDelayed(this, 250)
        }
    }

    private fun reset() { since = 0; count = 0; lastCpu = null; pss = null; vcpu = null; workerCpu = null; storageSamples.clear(); storage = null }

    private fun poll() {
        val current = engine() ?: run { reset(); main.post { snapshot = PerfSnapshot() }; return }
        if (current.asBinder() !== binder) { binder = current.asBinder(); reset() }
        val stats = try { current.frameStats(since) } catch (_: RemoteException) { return }
        val frames = stats.getLongArray("frames") ?: LongArray(0)
        for (i in frames.indices step 4) {
            val slot = count % published.size
            published[slot] = frames[i + 1]; presented[slot] = frames[i + 2]; work[slot] = frames[i + 3]
            count++; since = frames[i]
        }
        val now = stats.getLong("nowNs")
        stats.getLongArray("cpu")?.let { cpu ->
            val previous = lastCpu
            if (previous != null && now > lastCpuAt) {
                val wall = (now - lastCpuAt).toFloat()
                // Smooth over ~1 s: a 250 ms window of a 100 Hz tick counter is too coarse alone.
                if (cpu[0] >= 0 && previous[0] >= 0) vcpu = smooth(vcpu, 100f * (cpu[0] - previous[0]) / wall)
                if (cpu[3] >= 0 && previous[3] >= 0) workerCpu = smooth(workerCpu, 100f * (cpu[3] - previous[3]) / wall)
            }
            lastCpu = cpu; lastCpuAt = now
        }
        storage(now, stats.getLongArray("storage"))
        if (wantMemory && now - lastMemoryAt > 2_000_000_000L) {
            lastMemoryAt = now
            try { current.performance(1L).takeIf { it.containsKey("workerPssKiB") }?.let { pss = it.getLong("workerPssKiB") / 1024f } }
            catch (_: RemoteException) { }
        }
        val cpu = stats.getLongArray("cpu")
        val next = summarize(now, stats.getBundle("detailed"), cpu).copy(detailedError = stats.getString("detailedError"))
        main.post { snapshot = next }
    }

    private fun storage(now: Long, counters: LongArray?) {
        if (counters == null || counters.size < 8) { storageSamples.clear(); storage = null; return }
        storageSamples.addLast(now to counters)
        // Rates over the last second: steadier than one 250 ms poll apart, and still live.
        while (storageSamples.size > 2 && now - storageSamples[1].first >= 1_000_000_000L) storageSamples.removeFirst()
        val (at, old) = storageSamples.first()
        val seconds = (now - at) / 1e9
        if (seconds <= 0.1) return
        fun d(i: Int) = (counters[i] - old[i]).coerceAtLeast(0L).toDouble()
        val ops = d(0) + d(3) + d(6); val ns = d(2) + d(5) + d(7)
        storage = StorageRates(
            readBytes = d(1) / seconds, readOps = d(0) / seconds, writeBytes = d(4) / seconds, writeOps = d(3) / seconds,
            eraseOps = d(6) / seconds, busyPercent = ns / (seconds * 1e7), usPerOp = if (ops > 0) ns / ops / 1e3 else 0.0)
    }

    private fun smooth(previous: Float?, value: Float) = if (previous == null) value else previous * 0.6f + value * 0.4f

    private fun summarize(now: Long, detailed: Bundle?, cpu: LongArray?): PerfSnapshot {
        val n = minOf(count, published.size)
        fun at(back: Int) = (count - 1 - back).mod(published.size)
        // Frame intervals over the last 5 s, newest last.
        val intervals = ArrayList<Float>()
        var oneSecondFrames = 0
        var presentSum = 0f; var presentCount = 0; var latencySum = 0f
        for (back in 0 until n - 1) {
            val t = published[at(back)]
            if (now - t > 5_000_000_000L) break
            intervals.add((t - published[at(back + 1)]) / 1e6f)
            if (now - t <= 1_000_000_000L) {
                oneSecondFrames++
                if (presented[at(back)] > 0) {
                    presentSum += work[at(back)] / 1e6f; latencySum += (presented[at(back)] - t) / 1e6f; presentCount++
                }
            }
        }
        intervals.reverse()
        val stale = n == 0 || now - published[at(0)] > 1_000_000_000L
        val sorted = intervals.filter { it < IDLE_GAP_MS }.sorted()
        val worst = if (sorted.isEmpty()) 0f else sorted[((sorted.size - 1) * 0.99f).toInt()]
        val recent = intervals.takeLast(oneSecondFrames).filter { it < IDLE_GAP_MS }
        return PerfSnapshot(
            fps = if (stale) 0f else oneSecondFrames.toFloat(),
            frameMs = if (recent.isEmpty()) 0f else recent.average().toFloat(),
            lowFps = if (worst > 0f) 1000f / worst else 0f,
            maxFrameMs = sorted.lastOrNull() ?: 0f,
            presentMs = if (presentCount > 0) presentSum / presentCount else null,
            latencyMs = if (presentCount > 0) latencySum / presentCount else null,
            vcpuPercent = vcpu, vcpuCore = cpu?.get(1)?.takeIf { it >= 0 }?.toInt(),
            vcpuMHz = cpu?.get(2)?.takeIf { it > 0 }?.let { (it / 1000).toInt() }, workerPercent = workerCpu,
            pssMiB = pss, storage = storage,
            glMsPerFrame = detailed?.let { d -> d.getLong("glNs") / 1e6f / maxOf(1, d.getInt("frames")) },
            bqlMsPerFrame = detailed?.let { d -> d.getLong("bqlNs") / 1e6f / maxOf(1, d.getInt("frames")) },
            runPercent = detailed?.let { d -> 100f * d.getLong("runNs") / maxOf(1L, d.getLong("windowNs")) },
            intervals = intervals.toFloatArray(), detailedAvailable = detailed != null, idle = stale && n > 0)
    }

    fun close() { active = false; thread.quitSafely() }
}
