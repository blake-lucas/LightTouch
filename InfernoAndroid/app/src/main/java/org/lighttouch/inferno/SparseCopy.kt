package org.lighttouch.inferno

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.io.RandomAccessFile

class ImportCancelled : IOException("Импорт отменён")

/**
 * Copies a stream into a file and leaves holes where the source is zeros.
 *
 * The raw device disk is 34 GB of mostly nothing; copied byte for byte it fills
 * the phone. Whole zero blocks are skipped with a seek instead, so the file
 * reads the same and occupies only what holds data. The copy lands in
 * `name.part` first: a cancelled import never leaves a file that looks usable.
 */
object SparseCopy {
    private const val BLOCK = 1 shl 20

    fun copy(input: InputStream, dest: File, onBytes: (Int) -> Unit = {}, cancelled: () -> Boolean = { false }): Long {
        dest.parentFile?.mkdirs()
        val part = File(dest.parentFile, dest.name + ".part")
        var total = 0L
        try {
            RandomAccessFile(part, "rw").use { out ->
                out.setLength(0)
                val buf = ByteArray(BLOCK)
                while (true) {
                    if (cancelled()) throw ImportCancelled()
                    val n = readBlock(input, buf)
                    if (n <= 0) break
                    if (!isZero(buf, n)) {
                        out.seek(total)
                        out.write(buf, 0, n)
                    }
                    total += n
                    onBytes(n)
                }
                out.setLength(total)
                out.fd.sync()
            }
        } catch (e: Throwable) {
            part.delete()
            throw e
        }
        if (dest.isDirectory) dest.deleteRecursively()
        if (!part.renameTo(dest)) {
            dest.delete()
            if (!part.renameTo(dest)) throw IOException("не удалось переименовать ${part.name}")
        }
        return total
    }

    private fun readBlock(input: InputStream, buf: ByteArray): Int {
        var off = 0
        while (off < buf.size) {
            val n = input.read(buf, off, buf.size - off)
            if (n < 0) break
            off += n
        }
        return off
    }

    fun isZero(buf: ByteArray, n: Int): Boolean {
        for (i in 0 until n) if (buf[i].toInt() != 0) return false
        return true
    }
}
