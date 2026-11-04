package org.lighttouch.inferno

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.ByteArrayInputStream
import java.io.File

class SparseCopyTest {
    @get:Rule val tmp = TemporaryFolder()

    @Test fun zerosAndDataSurviveTheCopy() {
        val mb = 1 shl 20
        val source = ByteArray(5 * mb + 123)
        source[10] = 1
        source[3 * mb + 7] = 2
        source[source.size - 1] = 3
        val dest = File(tmp.root, "d/root")
        val n = SparseCopy.copy(ByteArrayInputStream(source), dest)
        assertEquals(source.size.toLong(), n)
        assertArrayEquals(source, dest.readBytes())
        assertFalse(File(dest.parentFile, "root.part").exists())
    }

    @Test fun trailingZerosKeepTheLength() {
        val source = ByteArray(3 * (1 shl 20))
        val dest = File(tmp.root, "blank")
        SparseCopy.copy(ByteArrayInputStream(source), dest)
        assertEquals(source.size.toLong(), dest.length())
    }

    @Test(expected = ImportCancelled::class)
    fun cancelLeavesNothing() {
        val dest = File(tmp.root, "x")
        try {
            SparseCopy.copy(ByteArrayInputStream(ByteArray(10)), dest, cancelled = { true })
        } finally {
            assertFalse(dest.exists())
            assertFalse(File(tmp.root, "x.part").exists())
        }
    }
}
