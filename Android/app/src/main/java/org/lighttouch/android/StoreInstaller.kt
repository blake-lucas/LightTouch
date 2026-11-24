package org.lighttouch.android

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.net.Uri
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.RemoteException
import android.os.SystemClock
import org.json.JSONObject
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

internal sealed class InstallRequest {
    class Catalog(val app: JSONObject) : InstallRequest()
    class Local(val uri: Uri) : InstallRequest()
}

/**
 * The Legacy Store's install queue wired to Android: one per process, so it outlives the activity (rotation, or
 * Android destroying a backgrounded one). While it has work it holds [DownloadService], which keeps the process
 * out of the freezer and the screen-off CPU awake and shows the progress notification; the activity keeps the
 * guest running for as long as [InstallQueue.active] (a paused guest never answers the installer).
 */
internal class StoreInstaller private constructor(private val context: Context) : QueueWork<InstallRequest> {
    private val apps = GuestApps(context)
    private val main = Handler(Looper.getMainLooper())
    val queue = InstallQueue(this)
    val available get() = apps.available

    /** The usbmux socket of the running guest, or null. Set by the activity, which polls the worker. */
    @Volatile var socket: String? = null
    /** LightTouch is on screen: no notification is needed to say a queue finished. */
    @Volatile var visible = false
    /** Posted to the main thread when the queue changes, and with the guest's app list after each install. */
    @Volatile var onChanged: (List<QueueItem>) -> Unit = {}
    @Volatile var onInstalled: (List<InstalledApp>) -> Unit = {}
    /** Run on the main thread when the queue has emptied; null while no activity exists. */
    @Volatile var onIdle: (() -> Unit)? = null
    /**
     * Set by the activity once it reaches the engine: holds (true) or releases its periodic storage sync. The sync made
     * the guest's writes wait, so uploads went in bursts, and an install cut short is lost anyway. A new connection is
     * told the current state, so a hold or release made while none existed still arrives.
     */
    @Volatile var storageSync: (Boolean) -> Unit = {}
        set(value) { field = value; value(holdingStorageSync) }
    @Volatile private var holdingStorageSync = false

    private val changePosted = AtomicBoolean(false)
    private var lastNotified = 0L
    @Volatile private var batchFirst = 0L
    private var exact: Pair<String, StoreTarget>? = null

    init {
        queue.onBusy = {
            batchFirst = queue.snapshot().filter { it.active }.minOfOrNull { it.id } ?: 0L
            DownloadService.hold(context, "Installing apps")
        }
        queue.onChange = {
            if (changePosted.compareAndSet(false, true)) main.post { changePosted.set(false); onChanged(queue.snapshot()) }
            notifyProgress()
        }
        queue.onIdle = {
            DownloadService.release(context)
            if (!visible) summary()?.let { DownloadService.finished(context, it) }
            main.post { onIdle?.invoke() ?: pauseGuest() }
        }
    }

    /** The items of the current run of installs (ones finished earlier in the session stay out of its numbers). */
    private fun batch() = queue.snapshot().filter { it.id >= batchFirst }

    private fun summary(): String? {
        val items = batch(); val done = items.count { it.state == QueueState.DONE }; val failed = items.count { it.state == QueueState.FAILED }
        return when {
            done == 0 && failed == 0 -> null
            failed == 0 -> if (done == 1) "Installed ${items.first { it.state == QueueState.DONE }.name}" else "Installed $done apps"
            done == 0 -> if (failed == 1) "Couldn't install ${items.first { it.state == QueueState.FAILED }.name}" else "Couldn't install $failed apps"
            else -> "$done installed, $failed failed"
        }
    }

    /** Text and progress bar for the ongoing notification, at most twice a second. */
    private fun notifyProgress() {
        val now = SystemClock.elapsedRealtime()
        if (now - lastNotified < 500) return
        val items = batch()
        val current = items.firstOrNull { it.state == QueueState.INSTALLING } ?: items.firstOrNull { it.state == QueueState.DOWNLOADING } ?: return
        lastNotified = now
        val position = (items.count { !it.active } + 1).coerceAtMost(items.size)
        DownloadService.update(context, (if (current.state == QueueState.INSTALLING) "Installing " else "Downloading ") + current.name +
            if (items.size > 1) " · $position of ${items.size}" else "", current.overall)
    }

    /** Normal behaviour once nothing is installing and nobody is looking: the guest pauses, as it does when LightTouch is left. */
    private fun pauseGuest() {
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName, binder: IBinder) {
                Thread({
                    try { IEngine.Stub.asInterface(binder).pause(true) } catch (_: RemoteException) { }
                    finally { context.unbindService(this) }
                }, "store-pause").start()
            }
            override fun onServiceDisconnected(name: ComponentName) = Unit
        }
        // Binds only to a guest worker that is still there.
        if (!context.bindService(Intent(context, EngineService::class.java), connection, 0)) context.unbindService(connection)
    }

    /**
     * The socket, waiting out a short pause: coming back from the file picker the guest is paused for a moment and
     * the activity clears the socket, so an IPA picked there reached its install step with none (seen on the Poco).
     */
    private fun liveSocket(cancelled: () -> Boolean): String? {
        val deadline = SystemClock.elapsedRealtime() + 15_000
        while (true) {
            socket?.takeIf { it.isNotEmpty() }?.let { return it }
            if (cancelled() || SystemClock.elapsedRealtime() > deadline) return null
            Thread.sleep(250)
        }
    }

    /** What the running guest is, asked once per guest. */
    private fun exactTarget(socket: String): StoreTarget = synchronized(this) {
        exact?.takeIf { it.first == socket }?.second ?: apps.target(socket).also { exact = socket to it }
    }

    override fun fetch(payload: InstallRequest, report: QueueReport, cancelled: () -> Boolean): File {
        val socket = liveSocket(cancelled) ?: throw StoreError("Start the guest to install apps.")
        report.update("Preparing", 0f)
        val target = exactTarget(socket)
        when (payload) {
            is InstallRequest.Catalog -> {
                var reported = 0L
                return LegacyStore.download(payload.app, target, File(context.cacheDir, "store"), { count, total ->
                    val now = SystemClock.elapsedRealtime()
                    if (now - reported >= 100 || count == total) {
                        reported = now
                        report.update("Downloading " + byteSize(count) + (total?.let { " of " + byteSize(it) } ?: ""), total?.let { count.toFloat() / it })
                        total?.let { report.overall(QueueStages.download(count.toFloat() / it)) }
                    }
                }, cancelled)
            }
            is InstallRequest.Local -> {
                val directory = File(context.cacheDir, "store/local-${System.nanoTime()}").apply { mkdirs() }
                try {
                    report.update("Copying", null)
                    val copy = File(directory, "Source.ipa")
                    context.contentResolver.openInputStream(payload.uri)?.use { input -> copy.outputStream().use { input.copyTo(it) } }
                        ?: throw StoreError("Cannot read the selected file.")
                    return copy
                } catch (e: Exception) { directory.deleteRecursively(); throw e }
            }
        }
    }

    override fun install(payload: InstallRequest, file: File, report: QueueReport, cancelled: () -> Boolean) {
        val socket = liveSocket(cancelled) ?: throw StoreError("The guest stopped. Start it to install apps.")
        val target = exactTarget(socket)
        report.update("Checking app", null)
        val prepared = File(file.parentFile, "Install.ipa")
        val info = IpaPreflight.prepare(file, prepared, target)
        if (payload is InstallRequest.Local) report.rename(info.name)
        if (cancelled()) throw StoreError("Cancelled")
        holdingStorageSync = true
        storageSync(true)
        try { apps.install(socket, prepared, info.bundleId, cancelled) { e ->
            when (e.optString("phase")) {
                "upload" -> { report.update("Copying to device", e.optInt("percent") / 100f); report.overall(QueueStages.copy(e.optInt("percent") / 100f)) }
                "mutation" -> { report.commit(); report.update("Installing", null) }
                // iOS's own steps are internal names; unpacking, most of a large app's time, shows
                // how much has landed (the service follows the guest's free space).
                "install" -> {
                    report.commit()
                    if (e.optLong("unpacked_total") > 0) {
                        val unpacked = e.optLong("unpacked").toFloat() / e.optLong("unpacked_total")
                        report.update("Unpacking · " + byteSize(e.optLong("unpacked")) + " of " + byteSize(e.optLong("unpacked_total")), unpacked)
                        report.overall(QueueStages.unpack(unpacked))
                    } else {
                        val percent = if (e.has("percent") && !e.isNull("percent")) e.optInt("percent") else null
                        report.update(if (e.optString("status") == "ExtractingPackage") "Unpacking" else "Installing", percent?.let { it / 100f })
                        if (percent != null && e.optString("status") != "ExtractingPackage") report.overall(QueueStages.iosPercent(percent))
                    }
                }
            }
        } } finally { holdingStorageSync = false; storageSync(false) }
        runCatching { apps.installedApps(socket) }.getOrNull()?.let { list -> main.post { onInstalled(list) } }
    }

    override fun discard(file: File) { file.parentFile?.deleteRecursively() }

    companion object {
        @Volatile private var instance: StoreInstaller? = null
        fun of(context: Context): StoreInstaller = instance ?: synchronized(this) {
            instance ?: StoreInstaller(context.applicationContext).also { instance = it }
        }
    }
}
