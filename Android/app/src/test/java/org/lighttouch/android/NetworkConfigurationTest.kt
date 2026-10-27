package org.lighttouch.android

import org.junit.Assert.*
import org.junit.Test
import java.time.LocalDate
import java.time.format.DateTimeFormatter

class NetworkConfigurationTest {
    @Test fun defaultsAreOfflineAndLocalNetworkRequiresOptIn() {
        val config = NetworkConfiguration()
        config.validate()
        assertFalse(config.enabled)
        assertFalse(config.localNetwork)
        assertEquals(0, config.nativeMode)
        assertEquals(1, config.copy(enabled = true).nativeMode)
        assertEquals(2, config.copy(enabled = true, route = NetworkRoute.ARCHIVE).nativeMode)
        assertEquals(0, config.copy(route = NetworkRoute.ARCHIVE).nativeMode)
    }

    @Test fun archiveUsesStrictCalendarDaysWithinSupportedRange() {
        for (date in listOf("19960101", "20000229", "20090909", LocalDate.now().format(DateTimeFormatter.BASIC_ISO_DATE))) {
            NetworkConfiguration(archiveDate = date).validate()
        }
        for (date in listOf("19951231", "20010229", "20090230", "20091301", "20090001", "2009099", "2009-09-09", "99991231")) {
            assertThrows("Must reject $date", Exception::class.java) { NetworkConfiguration(archiveDate = date).validate() }
        }
    }
}
