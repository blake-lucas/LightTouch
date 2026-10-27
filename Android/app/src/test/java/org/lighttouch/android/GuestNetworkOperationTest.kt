package org.lighttouch.android

import org.junit.Assert.*
import org.junit.Test

class GuestNetworkOperationTest {
    private class Guest {
        var clock = 0L
        var running = true
        var requests = 0
        var polls = 0
        val cancelled = mutableListOf<Long>()
        var admit: () -> Array<String> = { arrayOf("41", "", "0") }
        var complete: () -> Array<String> = { arrayOf("2", "0", "done", "") }
        val operation = GuestNetworkOperation(
            { _, _, _ -> requests++; admit() }, { id -> assertEquals(41L, id); polls++; complete() },
            { cancelled += it }, { running }, { clock }, { clock += it })
    }

    @Test fun competingRequestBetweenUploadsOnlyDelaysAdmission() {
        val guest = Guest()
        assertEquals("done", guest.operation.execute(9, "first chunk"))
        guest.admit = { if (guest.requests < 5) arrayOf("0", "guest operation already pending", "3") else arrayOf("41", "", "0") }
        assertEquals("done", guest.operation.execute(9, "second chunk"))
        assertEquals(5, guest.requests)
        assertEquals(2, guest.polls)
    }

    @Test fun bootReadinessAndAdmissionTimeoutAreBounded() {
        val guest = Guest()
        guest.admit = { arrayOf("0", "guest agent is absent or stale", "8") }
        assertThrows(IllegalStateException::class.java) { guest.operation.execute(0, admissionWaitMs = 300) }
        assertEquals(300L, guest.clock)
        assertEquals(0, guest.polls)
        assertTrue(guest.cancelled.isEmpty())
    }

    @Test fun invalidRequestIsNotRetried() {
        val guest = Guest()
        guest.admit = { arrayOf("0", "invalid chunk", "1") }
        assertThrows(IllegalStateException::class.java) { guest.operation.execute(9) }
        assertEquals(1, guest.requests)
    }

    @Test fun acceptedMutationIsNeverReplayedAfterLostCompletion() {
        for (reply in listOf(arrayOf("5", "0", "", ""), arrayOf("0", "0", "", "guest request no longer current"))) {
            val guest = Guest()
            guest.complete = { reply }
            assertThrows(IllegalStateException::class.java) { guest.operation.execute(7) }
            assertEquals(1, guest.requests)
            assertEquals(listOf(41L), guest.cancelled)
        }
    }

    @Test fun cancellationAndUnresponsivePollingReleaseAcceptedRequestWithoutReplay() {
        for (stop in listOf(false, true)) {
            val guest = Guest()
            guest.complete = { if (stop) guest.running = false; arrayOf("1", "0", "", "") }
            assertThrows(IllegalStateException::class.java) { guest.operation.execute(8) }
            assertEquals(1, guest.requests)
            assertEquals(listOf(41L), guest.cancelled)
        }
    }
}
