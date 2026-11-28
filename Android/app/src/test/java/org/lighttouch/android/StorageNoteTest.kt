package org.lighttouch.android

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Which devices give freed space back at Stop: the same cases as the desktop's details tests. */
class StorageNoteTest {
    @Test
    fun freedSpaceReturnsOnlyOnYaftlBoardsOnIos4To6() {
        assertTrue(freedSpaceReturnsAtStop("n81ap", "10B500"))
        assertTrue(freedSpaceReturnsAtStop("n88ap", "9B206"))
        assertTrue(freedSpaceReturnsAtStop("n18ap", "8C148"))
        assertFalse(freedSpaceReturnsAtStop("n72ap", "7E18"))
        assertFalse(freedSpaceReturnsAtStop("n72ap", "8C148"))
        assertFalse(freedSpaceReturnsAtStop("n42ap", "14G61"))
        assertFalse(freedSpaceReturnsAtStop("n88ap", "7E18"))
        assertTrue(storageNote("n42ap", "14G61").contains("not given back to your phone"))
    }
}
