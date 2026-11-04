package org.lighttouch.inferno

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.IBinder
import android.os.SystemClock
import android.provider.OpenableColumns
import androidx.documentfile.provider.DocumentFile
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.io.File
import java.io.FilterInputStream
import java.io.InputStream
import java.util.zip.ZipInputStream

data class ImportProgress(
    val running: Boolean = false,
    val title: String = "",
    val file: String = "",
    val done: Long = 0,
    val total: Long = 0,
    val message: String? = null,
    val failed: Boolean = false,
) {
    val fraction get() = if (total > 0) (done.toDouble() / total).toFloat().coerceIn(0f, 1f) else 0f
}

/** The import in progress, for the launcher (same process). */
object ImportState {
    private val _progress = MutableStateFlow(ImportProgress())
    val progress: StateFlow<ImportProgress> = _progress
    @Volatile var cancelRequested = false
    internal fun set(p: ImportProgress) { _progress.value = p }
    fun clearMessage() { _progress.value = _progress.value.copy(message = null, failed = false) }
}

/**
 * Copies a kit into the app's storage: a whole folder, a zip, or one file,
 * picked through the system's document picker. A foreground service, because
 * a 10–30 GB disk takes long enough that the user will switch away.
 */
class ImportService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val uri = intent?.data
        val kind = intent?.getStringExtra(EXTRA_KIND)
        if (uri == null || kind == null || ImportState.progress.value.running) {
            stopSelfResult(startId)
            return START_NOT_STICKY
        }
        startForeground(NOTIFICATION_ID, notification("Подготовка…", 0), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        ImportState.cancelRequested = false
        ImportState.set(ImportProgress(running = true, title = TITLES[kind] ?: "Импорт"))
        Thread({
            val result = runCatching { run(kind, uri) }
            val p = ImportState.progress.value
            ImportState.set(
                result.fold(
                    onSuccess = { p.copy(running = false, message = it, failed = false) },
                    onFailure = {
                        p.copy(running = false, failed = true,
                            message = if (it is ImportCancelled) "Импорт отменён" else "Ошибка: ${it.message ?: it.javaClass.simpleName}")
                    },
                ),
            )
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }, "inferno.import").start()
        return START_NOT_STICKY
    }

    private val config by lazy { Prefs.config(this) }

    private fun run(kind: String, uri: Uri): String = when (kind) {
        KIND_FOLDER -> importFolder(uri)
        KIND_ZIP -> importZip(uri)
        else -> importFile(uri)
    }

    // ---- a folder ----

    private fun importFolder(uri: Uri): String {
        val tree = DocumentFile.fromTreeUri(this, uri) ?: error("папка недоступна")
        val plan = mutableListOf<Pair<DocumentFile, File>>()

        val dataDir = findDir(tree, "InfernoData", 3)
            ?: tree.takeIf { looksLikeData(it) }
            ?: tree.listFiles().firstOrNull { it.isDirectory && looksLikeData(it) }
        dataDir?.let { collect(it, config.dataDirectory, plan) }
        findFile(tree, config.sepRom.name, 3)?.let { plan += it to config.sepRom }
        if (plan.isEmpty()) error("в папке нет ни InfernoData, ни AppleSEPROM-Cebu-B1")

        val total = plan.sumOf { it.first.length() }
        update(done = 0, total = total)
        var done = 0L
        for ((doc, dest) in plan) {
            update(file = dest.relativeTo(config.root).path)
            contentResolver.openInputStream(doc.uri)!!.use { input ->
                SparseCopy.copy(input, dest, { done += it; tick(done) }, { ImportState.cancelRequested })
            }
        }
        return "Скопировано файлов: ${plan.size} (${formatBytes(total)})"
    }

    private fun looksLikeData(dir: DocumentFile) =
        listOf("nvram", "sep_nvram", "root.qcow2", "root", "firmware").count { dir.findFile(it)?.isFile == true } >= 2

    private fun collect(dir: DocumentFile, dest: File, out: MutableList<Pair<DocumentFile, File>>) {
        for (child in dir.listFiles()) {
            val name = child.name ?: continue
            if (name.startsWith(".")) continue
            if (child.isDirectory) collect(child, File(dest, name), out) else out += child to File(dest, name)
        }
    }

    private fun findDir(dir: DocumentFile, name: String, depth: Int): DocumentFile? {
        if (dir.name.equals(name, ignoreCase = true)) return dir
        if (depth == 0) return null
        val children = dir.listFiles().filter { it.isDirectory }
        children.firstOrNull { it.name.equals(name, ignoreCase = true) }?.let { return it }
        return children.firstNotNullOfOrNull { findDir(it, name, depth - 1) }
    }

    private fun findFile(dir: DocumentFile, name: String, depth: Int): DocumentFile? {
        val children = dir.listFiles()
        children.firstOrNull { it.isFile && it.name == name }?.let { return it }
        if (depth == 0) return null
        return children.filter { it.isDirectory }.firstNotNullOfOrNull { findFile(it, name, depth - 1) }
    }

    // ---- a zip ----

    /**
     * Anything under an `InfernoData/` anywhere in the archive goes to
     * InfernoData, the SEP ROM goes beside it, and everything else is taken to
     * be InfernoData's own contents.
     */
    private fun importZip(uri: Uri): String {
        val total = sizeOf(uri)
        update(done = 0, total = total)
        var count = 0
        val counting = CountingStream(contentResolver.openInputStream(uri)!!) { tick(it) }
        ZipInputStream(counting.buffered(1 shl 16)).use { zip ->
            while (true) {
                val entry = zip.nextEntry ?: break
                val dest = zipDestination(entry.name)
                if (entry.isDirectory || dest == null) continue
                update(file = dest.relativeTo(config.root).path)
                SparseCopy.copy(zip, dest, {}, { ImportState.cancelRequested })
                count++
            }
        }
        if (count == 0) error("в архиве нет файлов")
        return "Распаковано файлов: $count"
    }

    private fun zipDestination(path: String): File? {
        val clean = path.replace('\\', '/').trimStart('/')
        if (clean.split('/').any { it == ".." } || clean.startsWith("__MACOSX/") || clean.substringAfterLast('/').startsWith("._")) return null
        if (clean.substringAfterLast('/') == config.sepRom.name) return config.sepRom
        val at = clean.indexOf("InfernoData/")
        val rest = if (at >= 0) clean.substring(at + "InfernoData/".length) else clean
        if (rest.isEmpty()) return null
        return File(config.dataDirectory, rest)
    }

    // ---- one file ----

    private fun importFile(uri: Uri): String {
        val name = displayName(uri) ?: error("у файла нет имени")
        val dest = if (name == config.sepRom.name) config.sepRom else File(config.dataDirectory, name)
        val total = sizeOf(uri)
        update(file = dest.relativeTo(config.root).path, done = 0, total = total)
        var done = 0L
        contentResolver.openInputStream(uri)!!.use { input ->
            SparseCopy.copy(input, dest, { done += it; tick(done) }, { ImportState.cancelRequested })
        }
        return "Добавлен ${dest.relativeTo(config.root).path}"
    }

    private fun displayName(uri: Uri): String? =
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }

    private fun sizeOf(uri: Uri): Long =
        contentResolver.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)?.use { c ->
            if (c.moveToFirst() && !c.isNull(0)) c.getLong(0) else 0L
        } ?: 0L

    // ---- progress ----

    private var lastTick = 0L

    private fun update(file: String? = null, done: Long? = null, total: Long? = null) {
        val p = ImportState.progress.value
        ImportState.set(p.copy(file = file ?: p.file, done = done ?: p.done, total = total ?: p.total))
    }

    private fun tick(done: Long) {
        val now = SystemClock.uptimeMillis()
        if (now - lastTick < 250) return
        lastTick = now
        update(done = done)
        val p = ImportState.progress.value
        getSystemService(NotificationManager::class.java)
            .notify(NOTIFICATION_ID, notification(p.file, (p.fraction * 100).toInt()))
    }

    private fun notification(text: String, percent: Int): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Импорт образа", NotificationManager.IMPORTANCE_LOW))
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat)
            .setContentTitle("Импорт образа — $percent%")
            .setContentText(text)
            .setProgress(100, percent, false)
            .setOngoing(true)
            .setContentIntent(open)
            .build()
    }

    private class CountingStream(input: InputStream, val onCount: (Long) -> Unit) : FilterInputStream(input) {
        var count = 0L
        override fun read(): Int = super.read().also { if (it >= 0) onCount(++count) }
        override fun read(b: ByteArray, off: Int, len: Int): Int =
            super.read(b, off, len).also { if (it > 0) { count += it; onCount(count) } }
    }

    companion object {
        const val EXTRA_KIND = "kind"
        const val KIND_FOLDER = "folder"
        const val KIND_ZIP = "zip"
        const val KIND_FILE = "file"
        private const val CHANNEL = "import"
        private const val NOTIFICATION_ID = 2
        private val TITLES = mapOf(KIND_FOLDER to "Импорт папки", KIND_ZIP to "Распаковка архива", KIND_FILE to "Импорт файла")

        fun start(context: Context, kind: String, uri: Uri) {
            context.startForegroundService(Intent(context, ImportService::class.java).setData(uri).putExtra(EXTRA_KIND, kind)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION))
        }
    }
}

fun formatBytes(bytes: Long): String {
    val units = listOf("Б", "КБ", "МБ", "ГБ", "ТБ")
    var v = bytes.toDouble()
    var i = 0
    while (v >= 1024 && i < units.lastIndex) { v /= 1024; i++ }
    return if (i == 0) "$bytes ${units[0]}" else String.format(java.util.Locale.ROOT, "%.1f %s", v, units[i])
}
