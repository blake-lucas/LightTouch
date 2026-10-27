package org.lighttouch.android

import android.content.Context
import android.os.Process
import android.system.Os
import android.system.OsConstants
import java.io.File

/**
 * The :engine worker's private USB link to its guest: the bundled QEMU-only usbmuxd
 * (libusbmuxd_qemu.so) listens on qemu.sock, which the engine dials through IT_USB_TCP, and
 * serves libimobiledevice clients on mux.sock. Same design as the desktop app's crates/lighttouch-services.
 * Owner-only sockets under filesDir; pairing records persist per device record. The daemon
 * gets SIGTERM when this process dies (LT_PARENT_DEATH_SIGNAL), and start() also retires
 * any daemon a previous, killed worker left behind.
 */
internal class UsbBridge(private val context: Context) {
    private val daemonBinary get() = File(context.applicationInfo.nativeLibraryDir, "libusbmuxd_qemu.so")
    private val runRoot get() = File(context.filesDir, "usb-run")
    private var process: java.lang.Process? = null
    @Volatile var muxSocket: String? = null; private set

    /** Null once the bridge is up; otherwise why not. The guest boots either way, without a cable if this failed. */
    fun start(recordId: String): String? {
        if (!daemonBinary.canExecute()) return "Guest app services are not included in this build."
        retireStale()
        val directory = File(runRoot, Process.myPid().toString())
        val pairing = File(context.filesDir, "usb-pairing/" + recordId.filter { it.isLetterOrDigit() })
        val logs = File(context.filesDir, "service-logs")
        for (dir in listOf(runRoot, directory, pairing, logs)) { dir.mkdirs(); Os.chmod(dir.absolutePath, 0x1c0) } // 0700
        // One log per bridge start, never removed before (185 on one phone): keep the newest few.
        logs.listFiles { f -> f.name.startsWith("usbmuxd-") }?.sortedByDescending { it.lastModified() }?.drop(7)?.forEach { it.delete() }
        val mux = File(directory, "mux.sock"); val qemu = File(directory, "qemu.sock")
        mux.delete(); qemu.delete()
        val builder = ProcessBuilder(daemonBinary.absolutePath, "-f", "-S", mux.absolutePath, "-C", pairing.absolutePath,
            "-P", File(directory, "daemon.pid").absolutePath)
            .redirectErrorStream(true).redirectOutput(File(logs, "usbmuxd-${Process.myPid()}.log"))
        builder.environment().apply {
            put("USBMUXD_QEMU_ADDR", qemu.absolutePath)
            put("LT_PARENT_DEATH_SIGNAL", "1"); put("LT_PARENT_PID", Process.myPid().toString())
        }
        val child = try { builder.start() } catch (e: java.io.IOException) { return "Guest USB bridge could not start: ${e.message}" }
        process = child
        val deadline = System.nanoTime() + 5_000_000_000L
        while (System.nanoTime() < deadline) {
            if (!child.isAlive) return "Guest USB bridge exited (${child.exitValue()}); see service-logs."
            if (privateSocket(mux) && privateSocket(qemu)) {
                // Read by the engine's USB controller when the guest brings the port up.
                Os.setenv("IT_USB_TCP", qemu.absolutePath, true)
                muxSocket = mux.absolutePath
                return null
            }
            Thread.sleep(25)
        }
        stop()
        return "Guest USB bridge did not create its sockets within 5 seconds."
    }

    private fun privateSocket(file: File): Boolean = try {
        val st = Os.lstat(file.absolutePath)
        OsConstants.S_ISSOCK(st.st_mode) && st.st_uid == Process.myUid() && st.st_mode and 0x3f == 0
    } catch (_: android.system.ErrnoException) { false }

    /** A worker killed with SIGKILL cannot clean up; its daemon got SIGTERM, but check anyway. */
    private fun retireStale() {
        for (dir in runRoot.listFiles().orEmpty()) {
            if (dir.name == Process.myPid().toString()) continue
            val pid = File(dir, "daemon.pid").takeIf { it.isFile }?.readText()?.trim()?.toIntOrNull()
            if (pid != null && File("/proc/$pid/cmdline").takeIf { it.exists() }?.readText()?.contains("libusbmuxd_qemu") == true)
                runCatching { Process.sendSignal(pid, OsConstants.SIGTERM) }
            dir.deleteRecursively()
        }
    }

    fun stop() {
        muxSocket = null
        process?.let { it.destroy(); if (!it.waitFor(2, java.util.concurrent.TimeUnit.SECONDS)) it.destroyForcibly() }
        process = null
    }
}
