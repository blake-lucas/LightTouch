package org.lighttouch.android

import org.xmlpull.v1.XmlPullParser
import java.io.File
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.zip.Inflater

class IpaError(message: String) : Exception(message)

/** What the installer needs from a checked IPA. */
data class IpaInfo(val bundleId: String, val name: String, val version: String?, val minimumOS: String?, val bytes: Long)

/**
 * As the desktop app's crates/lighttouch-catalog/src/ipa.rs: bounded ZIP, Info.plist and Mach-O checks against the
 * guest, then a copy whose main executable's Unix mode is 0755. Archives from old collections
 * often store no mode bits, and installd then installs an app SpringBoard cannot launch.
 * Never extracts the archive; the repair rewrites only central-directory fields of a copy.
 */
object IpaPreflight {
    private const val MAX_IPA = 2L shl 30
    private const val MAX_UNPACKED = 8L shl 30

    private class Entry(val name: String, val method: Int, val flags: Int, val compressed: Long, val size: Long,
                        val localOffset: Long, val externalAttr: Long, val centralOffset: Long) {
        val mode get() = (externalAttr ushr 16).toInt()
        val symlink get() = mode and 0xF000 == 0xA000
    }

    private class Zip(val file: RandomAccessFile, val entries: List<Entry>) {
        fun read(entry: Entry, limit: Long): ByteArray {
            if (entry.size > limit || entry.flags and 1 != 0) throw IpaError("The IPA contains an unsupported or oversized member: ${entry.name}")
            val header = ByteArray(30)
            file.seek(entry.localOffset); file.readFully(header)
            val b = ByteBuffer.wrap(header).order(ByteOrder.LITTLE_ENDIAN)
            if (b.getInt(0) != 0x04034b50) throw IpaError("The IPA has a damaged member header.")
            file.seek(entry.localOffset + 30 + (b.getShort(26).toInt() and 0xffff) + (b.getShort(28).toInt() and 0xffff))
            val raw = ByteArray(entry.compressed.toInt()); file.readFully(raw)
            return when (entry.method) {
                0 -> raw
                8 -> Inflater(true).run {
                    try {
                        setInput(raw); val out = ByteArray(entry.size.toInt()); var done = 0
                        while (done < out.size && !finished()) { val n = inflate(out, done, out.size - done); if (n == 0 && (needsInput() || needsDictionary())) break; done += n }
                        if (done != out.size) throw IpaError("The IPA member ${entry.name} is truncated.")
                        out
                    } finally { end() }
                }
                else -> throw IpaError("The IPA uses an unsupported compression method.")
            }
        }
    }

    private fun openZip(path: File): Zip {
        val file = RandomAccessFile(path, "r")
        try {
            val length = file.length()
            val tail = ByteArray(minOf(length, 65_557L).toInt())
            file.seek(length - tail.size); file.readFully(tail)
            val t = ByteBuffer.wrap(tail).order(ByteOrder.LITTLE_ENDIAN)
            val eocd = (tail.size - 22 downTo 0).firstOrNull { t.getInt(it) == 0x06054b50 } ?: throw IpaError("This file is not a ZIP archive.")
            var count = (t.getShort(eocd + 10).toInt() and 0xffff).toLong()
            var cdSize = t.getInt(eocd + 12).toLong() and 0xffffffffL
            var cdOffset = t.getInt(eocd + 16).toLong() and 0xffffffffL
            if (count == 0xffffL || cdOffset == 0xffffffffL || cdSize == 0xffffffffL) {
                // ZIP64: the locator sits immediately before the classic record.
                val locator = eocd - 20
                if (locator < 0 || t.getInt(locator) != 0x07064b50) throw IpaError("The IPA has a damaged ZIP64 directory.")
                val record = ByteArray(56); file.seek(t.getLong(locator + 8)); file.readFully(record)
                val r = ByteBuffer.wrap(record).order(ByteOrder.LITTLE_ENDIAN)
                if (r.getInt(0) != 0x06064b50) throw IpaError("The IPA has a damaged ZIP64 directory.")
                count = r.getLong(32); cdSize = r.getLong(40); cdOffset = r.getLong(48)
            }
            if (count > 100_000 || cdSize > 64L shl 20 || cdOffset + cdSize > length) throw IpaError("The IPA directory is too large or damaged.")
            val cd = ByteArray(cdSize.toInt()); file.seek(cdOffset); file.readFully(cd)
            val c = ByteBuffer.wrap(cd).order(ByteOrder.LITTLE_ENDIAN)
            val entries = ArrayList<Entry>(count.toInt())
            var at = 0
            repeat(count.toInt()) {
                if (at + 46 > cd.size || c.getInt(at) != 0x02014b50) throw IpaError("The IPA directory is damaged.")
                val nameLength = c.getShort(at + 28).toInt() and 0xffff
                val extraLength = c.getShort(at + 30).toInt() and 0xffff
                val commentLength = c.getShort(at + 32).toInt() and 0xffff
                var compressed = c.getInt(at + 20).toLong() and 0xffffffffL
                var size = c.getInt(at + 24).toLong() and 0xffffffffL
                var local = c.getInt(at + 42).toLong() and 0xffffffffL
                var extra = at + 46 + nameLength
                val extraEnd = extra + extraLength
                while (extra + 4 <= extraEnd) { // ZIP64 extended sizes replace saturated fields in order.
                    val id = c.getShort(extra).toInt() and 0xffff; val len = c.getShort(extra + 2).toInt() and 0xffff
                    if (id == 1) {
                        var p = extra + 4
                        if (size == 0xffffffffL) { size = c.getLong(p); p += 8 }
                        if (compressed == 0xffffffffL) { compressed = c.getLong(p); p += 8 }
                        if (local == 0xffffffffL) local = c.getLong(p)
                    }
                    extra += 4 + len
                }
                entries += Entry(String(cd, at + 46, nameLength, Charsets.UTF_8), c.getShort(at + 10).toInt() and 0xffff,
                    c.getShort(at + 8).toInt() and 0xffff, compressed, size, local, c.getInt(at + 38).toLong() and 0xffffffffL, cdOffset + at)
                at = extraEnd + commentLength
            }
            return Zip(file, entries)
        } catch (e: Exception) {
            file.close()
            throw if (e is IpaError) e else IpaError("The IPA is incomplete or damaged: ${e.message}")
        }
    }

    /** Minimal plist reader (binary bplist00 and XML) for Info.plist's top-level keys. */
    private fun plist(data: ByteArray): Map<String, Any?> {
        val root = if (data.size >= 8 && String(data, 0, 8, Charsets.US_ASCII) == "bplist00") binaryPlist(data) else xmlPlist(data)
        @Suppress("UNCHECKED_CAST")
        return root as? Map<String, Any?> ?: throw IpaError("The app's Info.plist is not a dictionary.")
    }

    private fun binaryPlist(data: ByteArray): Any? {
        val b = ByteBuffer.wrap(data).order(ByteOrder.BIG_ENDIAN)
        val trailer = data.size - 32
        val offsetSize = data[trailer + 6].toInt(); val refSize = data[trailer + 7].toInt()
        val objects = b.getLong(trailer + 8).toInt(); val top = b.getLong(trailer + 16).toInt(); val table = b.getLong(trailer + 24).toInt()
        fun uint(at: Int, size: Int): Int { var v = 0L; for (i in 0 until size) v = (v shl 8) or (data[at + i].toLong() and 0xff); return v.toInt() }
        fun offset(ref: Int) = uint(table + ref * offsetSize, offsetSize)
        fun parse(ref: Int, depth: Int): Any? {
            if (ref !in 0 until objects || depth > 32) throw IpaError("The app's Info.plist is damaged.")
            var at = offset(ref)
            val marker = data[at].toInt() and 0xff
            val type = marker shr 4
            var count = marker and 0xf
            at++
            if (type in setOf(0x5, 0x6, 0xA, 0xD, 0x4) && count == 0xf) {
                val intMarker = data[at].toInt() and 0xff
                val bytes = 1 shl (intMarker and 0xf)
                count = uint(at + 1, bytes); at += 1 + bytes
            }
            return when (type) {
                0x0 -> when (marker) { 0x08 -> false; 0x09 -> true; else -> null }
                0x1 -> { val bytes = 1 shl (marker and 0xf); var v = 0L; for (i in 0 until bytes) v = (v shl 8) or (data[at + i].toLong() and 0xff); v }
                0x2 -> if (marker and 0xf == 3) b.getDouble(at) else b.getFloat(at).toDouble()
                0x4 -> data.copyOfRange(at, at + count)
                0x5 -> String(data, at, count, Charsets.US_ASCII)
                0x6 -> String(data, at, count * 2, Charsets.UTF_16BE)
                0xA -> (0 until count).map { parse(uint(at + it * refSize, refSize), depth + 1) }
                0xD -> (0 until count).associate {
                    (parse(uint(at + it * refSize, refSize), depth + 1) as? String ?: throw IpaError("The app's Info.plist is damaged.")) to
                        parse(uint(at + (count + it) * refSize, refSize), depth + 1)
                }
                else -> null // Dates, UIDs and sets are irrelevant to these checks.
            }
        }
        return try { parse(top, 0) } catch (e: IndexOutOfBoundsException) { throw IpaError("The app's Info.plist is damaged.") }
    }

    private fun xmlPlist(data: ByteArray): Any? {
        val parser = android.util.Xml.newPullParser()
        parser.setFeature(XmlPullParser.FEATURE_PROCESS_DOCDECL, false)
        parser.setInput(data.inputStream(), null)
        fun value(): Any? {
            while (parser.eventType != XmlPullParser.START_TAG) parser.next()
            return when (parser.name) {
                "dict" -> buildMap {
                    while (parser.next() != XmlPullParser.END_TAG) {
                        if (parser.eventType != XmlPullParser.START_TAG) continue
                        val key = parser.nextText(); parser.next(); put(key, value())
                    }
                }
                "array" -> buildList { while (parser.next() != XmlPullParser.END_TAG) if (parser.eventType == XmlPullParser.START_TAG) add(value()) }
                "string" -> parser.nextText()
                "integer" -> parser.nextText().trim().toLongOrNull()
                "real" -> parser.nextText().trim().toDoubleOrNull()
                "true" -> { parser.next(); true }
                "false" -> { parser.next(); false }
                else -> { parser.nextText(); null }
            }
        }
        while (parser.next() != XmlPullParser.START_TAG || parser.name == "plist") { if (parser.eventType == XmlPullParser.END_DOCUMENT) return null }
        return value()
    }

    /** ipa.py _arm_slices: (cpu subtype, minimum iOS from load commands) for each ARM32 slice. */
    private fun armSlices(data: ByteArray): List<Pair<Int, List<Int>?>> {
        if (data.size < 28) throw IpaError("The app executable is truncated.")
        val big = ByteBuffer.wrap(data).order(ByteOrder.BIG_ENDIAN)
        if (big.getInt(0) == 0xcafebabe.toInt()) {
            val count = big.getInt(4)
            if (count !in 1..32 || data.size < 8 + count * 20) throw IpaError("Invalid universal executable.")
            return (0 until count).mapNotNull { i ->
                val cpu = big.getInt(8 + i * 20); val offset = big.getInt(16 + i * 20); val size = big.getInt(20 + i * 20)
                if (offset < 8 + count * 20 || size < 28 || offset.toLong() + size > data.size) throw IpaError("Invalid universal executable slices.")
                if (cpu == 12) armSlice(data.copyOfRange(offset, offset + size)) else null
            }.ifEmpty { throw IpaError("This IPA does not contain a supported ARM executable.") }
        }
        return listOf(armSlice(data))
    }

    private fun armSlice(data: ByteArray): Pair<Int, List<Int>?> {
        val b = ByteBuffer.wrap(data).order(ByteOrder.LITTLE_ENDIAN)
        if (data.size < 28 || b.getInt(0) != 0xfeedface.toInt()) throw IpaError("This IPA needs an unsupported executable format.")
        val cpu = b.getInt(4); val subtype = b.getInt(8) and 0xffffff; val fileType = b.getInt(12)
        val commands = b.getInt(16); val size = b.getInt(20)
        if (cpu != 12 || fileType != 2 || commands !in 0..65536 || 28L + size > data.size) throw IpaError("This IPA does not contain a supported ARM executable.")
        var cursor = 28; var minimum: List<Int>? = null
        repeat(commands) {
            if (cursor + 8 > 28 + size) throw IpaError("Invalid executable load commands.")
            val command = b.getInt(cursor); val length = b.getInt(cursor + 4)
            if (length < 8 || cursor + length > 28 + size) throw IpaError("Invalid executable load commands.")
            if (command == 0x21 && (length < 20 || b.getInt(cursor + 16) != 0)) throw IpaError("This app is encrypted; select a decrypted IPA.")
            val packed = when (command) { 0x25 -> b.getInt(cursor + 8); 0x32 -> if (b.getInt(cursor + 8) != 2) throw IpaError("The app executable targets an unsupported platform.") else b.getInt(cursor + 12); else -> null }
            if (packed != null) {
                val v = listOf(packed ushr 16, (packed ushr 8) and 255, packed and 255)
                if (minimum == null || LegacyStore.newer(v, minimum!!)) minimum = v
            }
            cursor += length
        }
        return subtype to minimum
    }

    /** Check an IPA against the target, then write the install copy (executable mode repaired) to [destination]. */
    fun prepare(source: File, destination: File, target: StoreTarget): IpaInfo {
        if (!source.isFile || source.length() !in 1..MAX_IPA) throw IpaError("Select a regular IPA smaller than 2 GiB.")
        val zip = openZip(source)
        val (info, executable) = zip.file.use {
            val entries = zip.entries
            if (entries.sumOf { it.size } > MAX_UNPACKED) throw IpaError("The unpacked IPA exceeds the supported size.")
            val names = HashSet<String>()
            for (entry in entries) {
                val parts = entry.name.split('/')
                if (!names.add(entry.name) || entry.name.startsWith("/") || ".." in parts || '\\' in entry.name || entry.flags and 1 != 0)
                    throw IpaError("The IPA contains unsafe, duplicate or encrypted ZIP members.")
                if (entry.symlink) {
                    val link = zip.read(entry, 4096).decodeToString()
                    val resolved = parts.dropLast(1).toMutableList()
                    if (link.isEmpty() || link.startsWith("/") || '\\' in link || '\u0000' in link || parts.size < 3 || parts[0] != "Payload" || !parts[1].endsWith(".app"))
                        throw IpaError("The IPA contains an unsafe symbolic link.")
                    for (component in link.split('/')) when (component) {
                        ".." -> { if (resolved.size <= 2) throw IpaError("The IPA contains a symbolic link outside its app."); resolved.removeAt(resolved.lastIndex) }
                        ".", "" -> Unit
                        else -> resolved += component
                    }
                }
            }
            val candidates = entries.filter { val p = it.name.split('/'); p.size == 3 && p[0] == "Payload" && p[1].endsWith(".app") && p[2] == "Info.plist" }
            if (candidates.size != 1) throw IpaError("Select an IPA containing one Payload app and Info.plist.")
            val info = plist(zip.read(candidates[0], 1L shl 20))
            val bundle = info["CFBundleIdentifier"] as? String
            val executableName = info["CFBundleExecutable"] as? String
            if (bundle.isNullOrEmpty() || executableName.isNullOrEmpty() || '/' in executableName || executableName == "." || executableName == "..")
                throw IpaError("The app has invalid bundle or executable metadata.")
            val executable = entries.firstOrNull { it.name == candidates[0].name.substringBeforeLast('/') + "/" + executableName }
                ?: throw IpaError("The app's executable is missing.")
            if (executable.symlink) throw IpaError("The app executable is unsupported.")
            val supported = if (target.arch == "armv6") setOf(0, 6) else setOf(0, 6, 9)
            val matching = armSlices(zip.read(executable, 256L shl 20)).filter { it.first in supported }
            if (matching.isEmpty()) throw IpaError("This app needs a newer processor than the guest.")
            // iOS runs the most specific slice it can; an older slice's lower minimum does not count.
            val best = matching.maxOf { it.first }
            val guest = LegacyStore.version(target.ios) ?: throw IpaError("Invalid guest iOS version.")
            matching.filter { it.first == best }.mapNotNull { it.second }.forEach {
                if (LegacyStore.newer(it, guest)) throw IpaError("The app executable requires iOS ${it.joinToString(".")}.")
            }
            val minimum = info["MinimumOSVersion"] as? String
            if (minimum != null) {
                val need = LegacyStore.version(minimum) ?: throw IpaError("The minimum iOS version could not be verified.")
                if (LegacyStore.newer(need, guest)) throw IpaError("This app requires iOS $minimum; the guest runs ${target.ios}.")
            }
            (info["UIDeviceFamily"] as? List<*>)?.let { families ->
                if (families.isNotEmpty() && families.none { it.toString() == target.family }) throw IpaError("This app is not made for the guest device family.")
            }
            IpaInfo(bundle, (info["CFBundleDisplayName"] as? String) ?: (info["CFBundleName"] as? String) ?: bundle,
                info["CFBundleVersion"] as? String, minimum, source.length()) to executable
        }
        source.copyTo(destination, overwrite = true)
        if (executable.mode and 0b001_001_001 == 0) RandomAccessFile(destination, "rw").use { out ->
            // Central directory: "version made by" host byte 3 (Unix), external attributes S_IFREG|0755.
            out.seek(executable.centralOffset + 5); out.write(3)
            val attr = ((0x81edL shl 16) or (executable.externalAttr and 0xffff)).toInt()
            out.seek(executable.centralOffset + 38)
            out.write(byteArrayOf(attr.toByte(), (attr ushr 8).toByte(), (attr ushr 16).toByte(), (attr ushr 24).toByte()))
            out.fd.sync()
        }
        return info
    }
}
