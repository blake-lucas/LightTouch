package org.lighttouch.inferno

import android.os.Handler
import android.os.HandlerThread
import android.view.Surface

/**
 * The engine in this process (always :vm). Thin wrappers over
 * src/main/cpp/inferno_jni.c; see there for what each one does.
 */
object Engine {
    const val STATE_IDLE = 0
    const val STATE_STARTING = 1
    const val STATE_RUNNING = 2
    const val STATE_STOPPED = 3

    /** The machine's buttons, wired to F-keys (hw/input/buttons.c). */
    const val KEY_VOLUME_DOWN = 3
    const val KEY_VOLUME_UP = 4
    const val KEY_POWER = 5
    const val KEY_HOME = 6

    init { System.loadLibrary("inferno_jni") }

    /**
     * Touches and buttons take the engine's big lock, which the guest's cores
     * may hold for a while. They go out from a thread of their own so the UI
     * never waits on it, and in order.
     */
    private val input: Handler by lazy { Handler(HandlerThread("inferno.input").apply { start() }.looper) }

    @Volatile var exitListener: ((Int) -> Unit)? = null
    @Volatile var hapticListener: ((level: Float, frequency: Float, durationMs: Int) -> Unit)? = null

    /** Null when the engine opened, otherwise why it did not. */
    fun load(): String? = nativeLoad()
    fun start(args: List<String>, log: String, workDir: String) = nativeStart(args.toTypedArray(), log, workDir)
    val state get() = nativeState()
    fun requestShutdown() = nativeRequestShutdown()
    fun setSurface(surface: Surface?) = nativeSetSurface(surface)
    fun screenSize(): IntArray = nativeScreenSize()
    fun touch(x: Int, y: Int, pressed: Boolean) { input.post { nativeTouch(x, y, pressed) } }
    fun key(number: Int, pressed: Boolean) { input.post { nativeKey(number, pressed) } }
    fun battery(percent: Int, external: Boolean, charging: Boolean) = nativeBattery(percent, external, charging)
    fun networkUp() = nativeNetUp()
    /** {frames shown here, frames the guest presented} since the last call. */
    fun stats(): LongArray = nativeStats()

    @JvmStatic fun onVmExit(status: Int) { exitListener?.invoke(status) }
    @JvmStatic fun onHaptic(level: Float, frequency: Float, durationMs: Int) { hapticListener?.invoke(level, frequency, durationMs) }

    @JvmStatic private external fun nativeLoad(): String?
    @JvmStatic private external fun nativeStart(args: Array<String>, log: String, workDir: String): Boolean
    @JvmStatic private external fun nativeState(): Int
    @JvmStatic private external fun nativeRequestShutdown()
    @JvmStatic private external fun nativeSetSurface(surface: Surface?)
    @JvmStatic private external fun nativeScreenSize(): IntArray
    @JvmStatic private external fun nativeTouch(x: Int, y: Int, pressed: Boolean)
    @JvmStatic private external fun nativeKey(number: Int, pressed: Boolean)
    @JvmStatic private external fun nativeBattery(percent: Int, external: Boolean, charging: Boolean)
    @JvmStatic private external fun nativeNetUp(): Boolean
    @JvmStatic private external fun nativeStats(): LongArray
}
