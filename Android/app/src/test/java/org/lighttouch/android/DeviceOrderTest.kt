package org.lighttouch.android

import org.junit.Assert.assertEquals
import org.junit.Test

/** The devices sheet's drag to reorder (List.moved); the order the library keeps is the core's (Contracts.cpp). */
class DeviceOrderTest {
    private val list = listOf("a", "b", "c", "d")

    @Test fun movesDownAndUp() {
        assertEquals(listOf("b", "c", "a", "d"), list.moved(0, 2))
        assertEquals(listOf("b", "c", "d", "a"), list.moved(0, 3))
        assertEquals(listOf("d", "a", "b", "c"), list.moved(3, 0))
        assertEquals(listOf("a", "c", "b", "d"), list.moved(2, 1))
    }

    @Test fun staysPutOrClamps() {
        assertEquals(list, list.moved(1, 1))
        assertEquals(list, list.moved(7, 0))
        assertEquals(list, list.moved(-1, 0))
        assertEquals(listOf("b", "c", "d", "a"), list.moved(0, 99))
        assertEquals(listOf("c", "a", "b", "d"), list.moved(2, -5))
        assertEquals(emptyList<String>(), emptyList<String>().moved(0, 0))
    }
}
