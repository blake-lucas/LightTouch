package org.lighttouch.android

import android.content.Context
import android.os.Build
import android.os.Process
import java.io.File
import java.io.FileOutputStream
import java.io.OutputStream
import java.io.RandomAccessFile
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * The logs in `files/logs` (docs/LOGS.md): each capped as a head (`NAME.log`) and two rolling tail segments
 * (`NAME.tail.log`, `NAME.tail.1.log`), with one previous generation (`NAME.prev*.log`). The engine's log is
 * written by lighttouch_jni.cpp to the same scheme; this side writes `app` and `prepare`, and reads them all.
 */
internal object Logs {
    const val KIB = 1024L
    const val MIB = 1024 * KIB

    class Spec(val name: String, val head: Long, val segment: Long)
    val APP = Spec("app", 256 * KIB, 256 * KIB)
    val PREPARE = Spec("prepare", MIB, MIB)
    /** Written natively (lighttouch_jni.cpp, ENGINE_LOG_*); listed for the export. */
    val ENGINE = Spec("engine", MIB, MIB)

    fun dir(context: Context): File = File(context.filesDir, "logs").apply { mkdirs() }

    private fun parts(base: String) = listOf("$base.log", "$base.tail.1.log", "$base.tail.log")

    /** A log's files in order (head, older tail, newest tail); `previous` reads the generation before. */
    fun files(dir: File, name: String, previous: Boolean = false): List<File> =
        parts(if (previous) "$name.prev" else name).map { File(dir, it) }.filter { it.isFile }

    /** Starts a generation: the current files become `NAME.prev*`, replacing the ones before. */
    fun newGeneration(dir: File, name: String) {
        parts("$name.prev").forEach { File(dir, it).delete() }
        for (part in parts(name)) File(dir, part).takeIf { it.exists() }?.renameTo(File(dir, "$name.prev" + part.removePrefix(name)))
    }

    /** The log's parts joined, the newest [max] bytes when the whole is larger (keeping the head first). */
    fun copyTo(files: List<File>, out: OutputStream, max: Long = Long.MAX_VALUE) {
        var budget = max
        for (file in files) {
            if (budget <= 0) break
            val take = minOf(file.length(), budget)
            RandomAccessFile(file, "r").use { input ->
                input.seek(0)
                val buffer = ByteArray(64 * 1024)
                var left = take
                while (left > 0) {
                    val n = input.read(buffer, 0, minOf(buffer.size.toLong(), left).toInt())
                    if (n <= 0) break
                    out.write(buffer, 0, n); left -= n
                }
            }
            budget -= take
        }
    }

    /** The last [max] bytes across a log's parts, as text (for crash summaries). */
    fun tail(files: List<File>, max: Long): String {
        val chunks = ArrayDeque<ByteArray>()
        var need = max
        for (file in files.reversed()) {
            if (need <= 0) break
            val take = minOf(file.length(), need)
            RandomAccessFile(file, "r").use { input ->
                input.seek(file.length() - take)
                chunks.addFirst(ByteArray(take.toInt()).also { input.readFully(it) })
            }
            need -= take
        }
        return chunks.joinToString("") { String(it, Charsets.UTF_8) }
    }

    /**
     * Appends to a capped log: into the head until it is full, then the tail, which rolls into `.tail.1` past its
     * segment size. Each write opens the file in append mode, so several processes may share a log (the app log)
     * and nothing is lost if this process dies.
     */
    class Writer(private val dir: File, private val spec: Spec) {
        private var segments = 0
        @Synchronized fun write(bytes: ByteArray, offset: Int = 0, length: Int = bytes.size) {
            try {
                dir.mkdirs()
                val head = File(dir, "${spec.name}.log")
                val target = if (head.length() < spec.head) head else tailFile()
                FileOutputStream(target, true).use { it.write(bytes, offset, length) }
            } catch (_: Exception) {
                // A full disk or a removed folder must never take LightTouch down with it.
            }
        }
        private fun tailFile(): File {
            val tail = File(dir, "${spec.name}.tail.log")
            if (tail.length() >= spec.segment) {
                tail.renameTo(File(dir, "${spec.name}.tail.1.log"))
            }
            if (!tail.exists()) {
                if (segments == 0 && File(dir, "${spec.name}.tail.1.log").exists()) segments = 1
                segments++
                val time = SimpleDateFormat("HH:mm:ss", Locale.US).format(Date())
                FileOutputStream(tail, true).use { it.write("----- [log] tail segment $segments, $time -----\n".toByteArray()) }
            }
            return tail
        }
    }
}

/**
 * LightTouch's own log (`app`): what the app did and what went wrong, a line per event, from any of its processes.
 * The main process starts a generation when it opens ([start]); the worker processes [attach].
 */
internal object AppLog {
    @Volatile private var writer: Logs.Writer? = null
    private var processName = "main"
    private val stamp = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSS", Locale.US)

    /** The main process, when LightTouch opens: the previous run becomes `app.prev`. */
    @Synchronized fun start(context: Context) {
        if (writer != null) return
        val dir = Logs.dir(context)
        Logs.newGeneration(dir, Logs.APP.name)
        open(context, dir)
        i("LightTouch ${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE}, ${BuildConfig.GIT_COMMIT}${if (BuildConfig.DEBUG) ", debug" else ""}) opened")
        i("Android ${Build.VERSION.RELEASE} (API ${Build.VERSION.SDK_INT}, ${Build.FINGERPRINT}); ${Build.MANUFACTURER} ${Build.MODEL}" +
            (if (Build.VERSION.SDK_INT >= 31) "; ${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}" else "; ${Build.HARDWARE}") +
            "; ${Build.SUPPORTED_ABIS.firstOrNull() ?: "?"}; page ${android.system.Os.sysconf(android.system.OsConstants._SC_PAGESIZE)}")
    }

    /** A worker process (`:engine`, `:firmware`): the same log, no new generation. */
    @Synchronized fun attach(context: Context, name: String) {
        if (writer != null) return
        processName = name
        open(context, Logs.dir(context))
    }

    private fun open(context: Context, dir: File) {
        writer = Logs.Writer(dir, Logs.APP)
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, error ->
            e("Crashed on thread ${thread.name}", error)
            previous?.uncaughtException(thread, error)
        }
    }

    fun i(message: String) = line('I', message)
    fun w(message: String) = line('W', message)
    fun e(message: String, error: Throwable? = null) = line('E', if (error == null) message else "$message: ${android.util.Log.getStackTraceString(error).trimEnd()}")

    private fun line(level: Char, message: String) {
        android.util.Log.println(when (level) { 'E' -> android.util.Log.ERROR; 'W' -> android.util.Log.WARN; else -> android.util.Log.INFO }, "LightTouch", message)
        val out = writer ?: return
        val time = synchronized(stamp) { stamp.format(Date()) }
        out.write("$time ${Process.myPid()} $processName $level $message\n".toByteArray())
    }
}
