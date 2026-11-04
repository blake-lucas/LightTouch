package org.lighttouch.inferno

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.os.BatteryManager
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.Process
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.io.File

/** What the :vm process is doing, for [VmActivity] (same process). */
object VmStatus {
    sealed interface State {
        data object Idle : State
        data object Booting : State
        data object Running : State
        data class Stopped(val status: Int) : State
        data class Failed(val reason: String) : State
    }
    private val _state = MutableStateFlow<State>(State.Idle)
    val state: StateFlow<State> = _state
    internal fun set(s: State) { _state.value = s }
}

/**
 * Owns the machine. Runs in the :vm process with the activity that shows it:
 * QEMU can be initialised only once per process and calls exit() on a bad
 * argument, so it gets a process of its own, the launcher survives whatever
 * happens here, and every start is a fresh process.
 */
class VmService : Service() {
    private val main = Handler(Looper.getMainLooper())
    private var wakeLock: PowerManager.WakeLock? = null
    private var batteryReceiver: BroadcastReceiver? = null
    private var stopping = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> start(intent)
            ACTION_STOP -> stop()
        }
        return START_NOT_STICKY
    }

    private fun start(intent: Intent) {
        startForeground(NOTIFICATION_ID, notification("Загрузка iOS…"), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        if (Engine.state != Engine.STATE_IDLE) return
        val args = intent.getStringArrayExtra(EXTRA_ARGS)?.toList() ?: return fail("нет аргументов запуска")
        val haptics = intent.getBooleanExtra(EXTRA_HAPTICS, true)
        val config = Prefs.config(this)

        Engine.load()?.let { return fail("Движок не загрузился: $it") }

        config.logDirectory.mkdirs()
        // The console log is appended to by the engine; start each run clean.
        runCatching { config.guestConsoleLog.writeText("") }
        Prefs.exitMarker(this).delete()

        Engine.exitListener = { status -> main.post { onExit(status) } }
        if (haptics) Engine.hapticListener = ::vibrate

        val socketDir = File(cacheDir, "sock").apply { mkdirs() }
        File(socketDir, VmConfig.USB_SOCKET).delete()
        if (!Engine.start(args, config.engineLog.path, socketDir.path)) return fail("Движок не запустился")

        VmStatus.set(VmStatus.State.Booting)
        wakeLock = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "inferno:vm").apply { acquire() }
        watchBattery()
        pollUntilRunning()
    }

    private fun pollUntilRunning() {
        main.postDelayed({
            when (Engine.state) {
                Engine.STATE_RUNNING -> {
                    VmStatus.set(VmStatus.State.Running)
                    notify("iOS работает")
                }
                Engine.STATE_STARTING -> pollUntilRunning()
            }
        }, 200)
    }

    /** Asks the machine to stop; if it does not within a while, the process goes. */
    private fun stop() {
        if (stopping) return
        stopping = true
        if (Engine.state == Engine.STATE_RUNNING) {
            notify("Выключение…")
            Engine.requestShutdown()
            main.postDelayed({ finishProcess() }, 15_000)
        } else {
            finishProcess()
        }
    }

    private fun onExit(status: Int) {
        Log.i("inferno", "машина остановлена ($status)")
        runCatching { Prefs.exitMarker(this).writeText(status.toString()) }
        VmStatus.set(VmStatus.State.Stopped(status))
        finishProcess()
    }

    private fun fail(reason: String) {
        Log.e("inferno", reason)
        runCatching {
            val config = Prefs.config(this)
            config.logDirectory.mkdirs()
            config.engineLog.writeText(reason + "\n")
        }
        VmStatus.set(VmStatus.State.Failed(reason))
        main.postDelayed({ finishProcess() }, 4000)
    }

    /** QEMU cannot be started twice in one process: the next start gets a new one. */
    private fun finishProcess() {
        wakeLock?.let { if (it.isHeld) it.release() }
        batteryReceiver?.let { runCatching { unregisterReceiver(it) } }
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        main.postDelayed({ Process.killProcess(Process.myPid()) }, 600)
    }

    /** The phone's own battery is the one the guest shows. */
    private fun watchBattery() {
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
                val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
                val plugged = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0
                val status = intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
                val charging = status == BatteryManager.BATTERY_STATUS_CHARGING
                if (level >= 0 && scale > 0) Engine.battery(level * 100 / scale, plugged, charging)
            }
        }
        batteryReceiver = receiver
        registerReceiver(receiver, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
    }

    private val vibrator: Vibrator by lazy { getSystemService(VibratorManager::class.java).defaultVibrator }

    private fun vibrate(level: Float, frequency: Float, durationMs: Int) {
        if (level <= 0f) { vibrator.cancel(); return }
        val amplitude = (level * 255).toInt().coerceIn(1, 255)
        val effect = if (vibrator.hasAmplitudeControl()) VibrationEffect.createOneShot(durationMs.toLong().coerceAtLeast(10), amplitude)
        else VibrationEffect.createOneShot(durationMs.toLong().coerceAtLeast(10), VibrationEffect.DEFAULT_AMPLITUDE)
        vibrator.vibrate(effect)
    }

    private fun notify(text: String) {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(text))
    }

    private fun notification(text: String): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Виртуальная машина", NotificationManager.IMPORTANCE_LOW))
        val open = PendingIntent.getActivity(this, 0, Intent(this, VmActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 1, Intent(this, VmService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE)
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat)
            .setContentTitle("iOS 14 · iPhone 11")
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Выключить", stop).build())
            .build()
    }

    companion object {
        const val ACTION_START = "org.lighttouch.inferno.START"
        const val ACTION_STOP = "org.lighttouch.inferno.STOP"
        const val EXTRA_ARGS = "args"
        const val EXTRA_HAPTICS = "haptics"
        private const val CHANNEL = "vm"
        private const val NOTIFICATION_ID = 1

        fun stop(context: Context) {
            context.startService(Intent(context, VmService::class.java).setAction(ACTION_STOP))
        }
    }
}
