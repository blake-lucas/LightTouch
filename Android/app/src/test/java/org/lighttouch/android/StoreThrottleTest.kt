package org.lighttouch.android

import org.junit.Assert.*
import org.junit.Test

class StoreThrottleTest {
    private val s = 1_000_000_000L

    @Test fun aBurstGoesAtOnceThenOnePerInterval() {
        val t = StoreThrottle { 0L }
        repeat(StoreThrottle.BURST) { assertEquals(0L, t.reserve(0L)) }
        assertEquals(StoreThrottle.INTERVAL_NS, t.reserve(0L))
        assertEquals(2 * StoreThrottle.INTERVAL_NS, t.reserve(0L))
        // Quiet time earns the burst back.
        assertEquals(0L, t.reserve(2 * (StoreThrottle.BURST + 2) * StoreThrottle.INTERVAL_NS))
    }

    @Test fun aPauseFollowsTheServerWithinLimits() {
        val t = StoreThrottle { 0L }
        assertNull(t.answered(404, null))
        assertNull(t.answered(503, null))
        assertEquals(StoreThrottle.DEFAULT_PAUSE_NS, t.answered(429, null))
        assertEquals("a shorter answer doesn't cut a pause short", StoreThrottle.DEFAULT_PAUSE_NS, t.answered(429, "5"))
        assertEquals(StoreThrottle.MAX_PAUSE_NS, t.answered(503, "86400"))
        assertNull(t.paused(StoreThrottle.MAX_PAUSE_NS))
    }

    @Test fun impatientRequestsFailDuringAPause() {
        val t = StoreThrottle { 0L }
        t.answered(429, "12")
        val e = assertThrows(StoreThrottle.Paused::class.java) { t.enter(patient = false) }
        assertEquals(12 * s, e.leftNs)
        assertEquals("Legacy Store asked LightTouch to slow down. Try again in 12 seconds.", e.message)
    }

    @Test fun theMessageRoundsUp() = assertEquals("Legacy Store asked LightTouch to slow down. Try again in 2 seconds.", StoreThrottle.slowDown(1_500_000_000L))
}
