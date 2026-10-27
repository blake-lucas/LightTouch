package org.lighttouch.android

import android.app.*
import android.content.Context
import android.content.Intent
import android.os.IBinder
import android.os.PowerManager

/**
 * Keeps the app's own process running while it downloads or copies an IPSW, or works through the Legacy Store's
 * install queue. Without a foreground service Android freezes a backgrounded app within seconds, and the transfer
 * fails. Held for each transfer or queue, gone after.
 */
class DownloadService : Service() {
    private var awake: PowerManager.WakeLock? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(3, notification(this, intent?.getStringExtra("text") ?: "Downloading iOS", null))
        // The screen may turn off during a long download.
        if (awake == null) awake = getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "LightTouch:download").apply { acquire(3 * 60 * 60 * 1000L) }
        return START_NOT_STICKY
    }

    override fun onTimeout(startId: Int, fgsType: Int) = stopSelf()
    override fun onDestroy() { awake?.let { if (it.isHeld) it.release() }; awake = null; super.onDestroy() }
    override fun onBind(intent: Intent): IBinder? = null

    companion object {
        private var holders = 0

        private fun manager(context: Context): NotificationManager = context.getSystemService(NotificationManager::class.java).also {
            it.createNotificationChannel(NotificationChannel("download", "Downloads", NotificationManager.IMPORTANCE_LOW))
            it.createNotificationChannel(NotificationChannel("installed", "Finished installs", NotificationManager.IMPORTANCE_DEFAULT))
        }
        private fun launch(context: Context) = PendingIntent.getActivity(context, 3, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)

        /** The ongoing notification; [fraction] fills its bar, and null leaves the bar out. */
        private fun notification(context: Context, text: String, fraction: Float?): Notification {
            manager(context)
            return Notification.Builder(context, "download").setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle("LightTouch").setContentText(text).setContentIntent(launch(context)).setOngoing(true)
                .apply { if (fraction != null) setProgress(100, (fraction.coerceIn(0f, 1f) * 100).toInt(), false) }.build()
        }

        /** Call from the foreground (Android refuses to start a foreground service from the background). */
        fun hold(context: Context, text: String) = synchronized(this) {
            holders++
            // A refused start leaves the work running without the service's protection.
            runCatching { context.startForegroundService(Intent(context, DownloadService::class.java).putExtra("text", text)) }
            Unit
        }

        fun release(context: Context) = synchronized(this) {
            if (holders > 0 && --holders == 0) {
                context.stopService(Intent(context, DownloadService::class.java))
                // An update posted while the service was refused (or not yet started) is a plain notification, which
                // stopping the service leaves behind.
                manager(context).cancel(3)
            }
        }

        /** New text and progress for the service's notification, while something holds it. */
        fun update(context: Context, text: String, fraction: Float?) = synchronized(this) {
            if (holders > 0) manager(context).notify(3, notification(context, text, fraction))
        }

        /** LightTouch is back on screen, where the Store shows how the installs went. */
        fun clearFinished(context: Context) = manager(context).cancel(4)

        /** A notification that stays after the service is gone: how a queue of installs ended. */
        fun finished(context: Context, text: String) {
            manager(context).notify(4, Notification.Builder(context, "installed").setSmallIcon(android.R.drawable.stat_sys_download_done)
                .setContentTitle("LightTouch").setContentText(text).setContentIntent(launch(context)).setAutoCancel(true).build())
        }
    }
}
