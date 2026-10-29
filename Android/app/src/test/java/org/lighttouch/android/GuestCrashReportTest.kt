package org.lighttouch.android

import org.junit.Assert.*
import org.junit.Test
import java.io.File

/** The crash answer must name the faulting frames, which the old header-only filter threw away. */
class GuestCrashReportTest {
    private val asphalt = """
        Hardware Model:      iPod4,1
        Process:             Asphalt8 [1234]
        Path:                /var/mobile/Applications/X/Asphalt8.app/Asphalt8

        Date/Time:           2026-10-29 07:54:41.123 +0000
        OS Version:          iOS 6.1.6 (10B500)

        Exception Type:  EXC_BAD_ACCESS (SIGSEGV)
        Exception Subtype: KERN_INVALID_ADDRESS at 0x00000000
        Crashed Thread:  0

        Thread 0 name:  Dispatch queue: com.apple.main-thread
        Thread 0 Crashed:
        0   libobjc.A.dylib                0x3a1b5f00 0x3a1b3000 + 12032
        1   Asphalt8                       0x0012f4c8 0x1000 + 1238728
        2   UIKit                          0x34f2a1f0 0x34f00000 + 172528

        Thread 0 crashed with ARM Thread State (32-bit):
            r0: 0x00000000    r1: 0x00000001

        Thread 1:
        0   libsystem_kernel.dylib         0x3a2a9e30 0x3a2a8000 + 7728

        Binary Images:
        0x1000 - 0x2a1fff +Asphalt8 armv7  <1a2b> /var/mobile/Applications/X/Asphalt8.app/Asphalt8
        0x3a1b3000 - 0x3a1c9fff  libobjc.A.dylib armv7  <aaaa> /usr/lib/libobjc.A.dylib
    """.trimIndent()

    private val dyld = """
        Process:             game [900]
        OS Version:          iOS 6.1.6 (10B500)

        Exception Type:  EXC_BREAKPOINT (SIGTRAP)
        Exception Codes: 0x0000000000000001, 0x000000000e7ff5a8
        Crashed Thread:  0

        Dyld Error Message:
          Symbol not found: _OBJC_CLASS_${'$'}_UIAlertController
          Referenced from: /var/mobile/Applications/X/game.app/game
          Expected in: /System/Library/Frameworks/UIKit.framework/UIKit

        Thread 0:
        0   dyld                           0x2fe0a000 0x2fe00000 + 40960
        1   game                           0x0000f000 0x1000 + 57344
    """.trimIndent()

    @Test fun keepsTheCrashedThreadsFramesNotJustTheHeader() {
        val report = GuestCrashReport.summarize("Asphalt8.plist", asphalt)
        assertTrue(report.body.contains("Thread 0 Crashed:"))
        assertTrue("the faulting frame must survive", report.body.contains("0x3a1b5f00"))
        assertTrue("the caller frame must survive", report.body.contains("0x0012f4c8"))
        assertFalse("another thread's frames are not the crash", report.body.contains("0x3a2a9e30"))
        assertTrue(report.body.contains("Faulted in: libobjc.A.dylib"))
        assertTrue(report.body.contains("Called from: Asphalt8"))
        assertTrue(report.verdict.contains("Asphalt8"))
    }

    @Test fun aMissingSymbolIsAVersionMismatch() {
        val report = GuestCrashReport.summarize("game.plist", dyld)
        assertTrue(report.verdict.contains("_OBJC_CLASS_\$_UIAlertController"))
        assertTrue(report.verdict.contains("newer iOS"))
        assertTrue(report.body.contains("Symbol not found"))
    }

    @Test fun aReportWithoutFramesStillGivesTheHeader() {
        val report = GuestCrashReport.summarize("bare.plist", "Process:  thing [1]\nOS Version:  iOS 6.1.6 (10B500)\n")
        assertTrue(report.body.contains("thing [1]"))
        assertTrue(report.verdict.contains("no faulting frame"))
    }

    @Test fun diagnosisIncludesTheEnginesOwnLogTail() {
        val state = File.createTempFile("state", "").let { it.delete(); it.mkdirs(); it }
        try {
            File(state, "engine-1.log").writeText((1..200).joinToString("\n") { "line $it" })
            val tail = GuestCrashDiagnosis.tail(GuestCrashDiagnosis.engineLog(state), 5)
            assertEquals(listOf("line 196", "line 197", "line 198", "line 199", "line 200"), tail)
            val text = GuestCrashDiagnosis.report(state, "Asphalt8.plist", asphalt, "dyld: missing _foo")
            assertTrue(text.contains("Guest system log"))
            assertTrue(text.contains("line 200"))
            assertTrue(text.contains("Thread 0 Crashed:"))
        } finally { state.deleteRecursively() }
    }
}
