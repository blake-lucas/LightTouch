package org.lighttouch.android

import org.junit.Assert.*
import org.junit.Test

class NetworkSessionTest {
    private val direct = NetworkConfiguration(enabled = true)
    private val archive = direct.copy(route = NetworkRoute.ARCHIVE)

    @Test fun switchesRoutesAndDatesWithoutRestarting() {
        val session = NetworkSession()
        session.begin(direct)
        for (config in listOf(archive, archive.copy(archiveDate = "20100101"), direct.copy(localNetwork = true))) {
            session.request(config)
            assertNotEquals(config, session.snapshot().active)
            session.refresh { assertEquals(config, it); 0 to "" }
            assertEquals(config, session.snapshot().active)
            assertEquals("", session.snapshot().notice)
        }
    }

    @Test fun startingEngineRetriesLatestSettingsAndReportsActualRoute() {
        val session = NetworkSession()
        session.begin(direct)
        session.request(archive)
        session.refresh { 8 to "Not ready" }
        assertEquals(direct, session.snapshot().active)
        assertTrue(session.snapshot().notice.contains("Waiting"))
        val newest = archive.copy(archiveDate = "20100101")
        session.request(newest)
        session.refresh { assertEquals(newest, it); 0 to "" }
        assertEquals(newest, session.snapshot().active)
        session.refresh { fail("Already applied; must not reconfigure"); 0 to "" }
    }

    @Test fun offlineBootTurnsOnLive() {
        val session = NetworkSession()
        session.begin(NetworkConfiguration())
        session.request(archive)
        session.refresh { assertEquals(archive, it); 0 to "" }
        assertEquals(archive, session.snapshot().active)
        assertEquals("", session.snapshot().notice)
    }

    @Test fun enabledBootCanDisableAndReenableWithoutNewGuestSetup() {
        val session = NetworkSession()
        session.begin(direct)
        for (config in listOf(direct.copy(enabled = false), archive)) {
            session.request(config)
            session.refresh { assertEquals(config, it); 0 to "" }
            assertEquals(config, session.snapshot().active)
        }
    }

    @Test fun rejectedPolicyPreservesActualStatusAndDoesNotLoop() {
        val session = NetworkSession()
        session.begin(direct)
        session.request(archive)
        session.refresh { 4 to "Unsupported engine" }
        assertEquals(direct, session.snapshot().active)
        assertTrue(session.snapshot().notice.contains("Unsupported engine"))
        session.refresh { fail("Hard failure must not loop"); 0 to "" }
    }

    @Test fun invalidDatesNeverReachNativePolicy() {
        val session = NetworkSession()
        session.begin(archive)
        assertThrows(Exception::class.java) { session.request(archive.copy(archiveDate = "20090230")) }
        session.refresh { fail("Invalid date must not be applied"); 0 to "" }
        assertEquals(archive, session.snapshot().active)
    }
}
