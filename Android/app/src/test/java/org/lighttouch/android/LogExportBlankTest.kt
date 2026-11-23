package org.lighttouch.android

import org.junit.Assert.*
import org.junit.Test

class LogExportBlankTest {
    private val apk = "/data/app/~~abc==/org.lighttouch.android-xyz=="
    private val pairs = listOf(apk to "<apk>", "/data/user/0/org.lighttouch.android" to "<data>")

    @Test fun aTraceKeepsItsLengthAndEveryHighByte() {
        val name = "$apk/lib/arm64/libqemu-arm.so".toByteArray()
        // protobuf-shaped: tag, varint length, the path, then a fixed64 full of high bytes
        val trace = byteArrayOf(0x32, name.size.toByte()) + name + byteArrayOf(0x11) + ByteArray(8) { (0xF0 + it).toByte() }
        val out = LogExport.blankPaths(trace, pairs)
        assertEquals(trace.size, out.size)
        assertEquals(name.size.toByte(), out[1])
        assertArrayEquals(trace.copyOfRange(trace.size - 9, trace.size), out.copyOfRange(out.size - 9, out.size))
        val path = String(out, 2, name.size)
        assertTrue(path, path.startsWith("<apk>____"))
        assertTrue(path, path.endsWith("/lib/arm64/libqemu-arm.so"))
        assertFalse(String(out, Charsets.ISO_8859_1).contains("~~abc"))
    }

    @Test fun everyOccurrenceIsBlanked() {
        val text = "a /data/user/0/org.lighttouch.android/files b /data/user/0/org.lighttouch.android".toByteArray()
        val out = String(LogExport.blankPaths(text, pairs))
        assertEquals(text.size, out.length)
        assertFalse(out.contains("org.lighttouch"))
        assertEquals(2, Regex("<data>_+").findAll(out).count())
    }
}
