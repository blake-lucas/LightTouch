package org.lighttouch.inferno

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.io.RandomAccessFile

class VmConfigTest {
    @get:Rule val tmp = TemporaryFolder()

    private fun kit(): VmConfig {
        val root = tmp.newFolder("files")
        val config = VmConfig(root)
        (VmConfig.REQUIRED_FILES.map { it.second } + listOf(
            "InfernoData/" + VmConfig.FALLBACK_FIRMWARE.kernel,
            "InfernoData/" + VmConfig.FALLBACK_FIRMWARE.deviceTree,
            "InfernoData/" + VmConfig.FALLBACK_FIRMWARE.trustcache,
        )).forEach { File(root, it).apply { parentFile!!.mkdirs(); writeText("x") } }
        return config
    }

    @Test fun missingUntilTheDiskIsThere() {
        val config = kit()
        assertEquals(listOf("Диск устройства (root.qcow2 или root)"), config.missing())
        File(config.dataDirectory, "root.qcow2").writeText("QFI")
        assertTrue(config.missing().isEmpty())
    }

    @Test fun aFolderIsNotAFile() {
        val config = kit()
        File(config.dataDirectory, "firmware").apply { delete(); mkdirs() }
        assertTrue("Прошивка NVMe" in config.missing())
    }

    @Test fun argumentsMatchInfernoIos() {
        val config = kit()
        File(config.dataDirectory, "root.qcow2").writeText("QFI")
        val d = config.dataDirectory.path
        val args = config.arguments(VmSettings(halfResolution = false), File("/q"))
        val machine = args[args.indexOf("-M") + 1]
        assertEquals(
            "t8030,usb-uplink-type=inferno,usb-uplink-addr=unix:inferno-usb.sock," +
                "trustcache=$d/Restore/Firmware/038-44135-124.dmg.trustcache,ticket=$d/root_ticket.der," +
                "sep-fw=$d/sep-firmware.n104.RELEASE.new.img4,sep-rom=${config.sepRom.path},kaslr-off=true," +
                "boot-mode=exit_recovery,disp-width=828,disp-height=1792,disp-scale=2",
            machine,
        )
        assertEquals("tcg,thread=multi,tb-size=128", args[args.indexOf("-accel") + 1])
        assertEquals(VmConfig.DEFAULT_BOOT_ARGS, args[args.indexOf("-append") + 1])
        assertEquals("4", args[args.indexOf("-smp") + 1])
        assertEquals("3G", args[args.indexOf("-m") + 1])
        assertEquals("none", args[args.indexOf("-display") + 1])
        assertTrue("file=$d/root.qcow2,format=qcow2,if=none,id=root" in args)
        assertTrue("nvme-ns,drive=panic_log,bus=nvme-bus.0,nsid=7,nstype=8,logical_block_size=4096,physical_block_size=4096" in args)
        assertTrue("apple-ncm-host,netdev=net0,conn-addr=inferno-usb.sock" in args)
        assertEquals("apple-nvram,drive=nvram,bus=nvme-bus.0,nsid=5,nstype=5,id=nvram,logical_block_size=4096,physical_block_size=4096", args.last())
        assertFalse(args.any { it.contains("id=xfer") })

        val half = config.arguments(VmSettings(network = false, bootArgs = "-v"), File("/q"))
        assertTrue(half[half.indexOf("-M") + 1].endsWith("disp-width=414,disp-height=896,disp-scale=1"))
        assertEquals("-v", half[half.indexOf("-append") + 1])
        assertFalse(half.any { it.startsWith("apple-ncm-host") })
    }

    @Test fun firmwareComesFromTheManifest() {
        val config = kit()
        val restore = File(config.dataDirectory, "Restore")
        listOf("kernelcache.release.iphone12", "Firmware/all_flash/DeviceTree.n104ap.im4p", "Firmware/099-1.dmg.trustcache")
            .forEach { File(restore, it).apply { parentFile!!.mkdirs(); writeText("x") } }
        File(restore, "BuildManifest.plist").writeText(
            """<?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            <plist version="1.0"><dict>
              <key>BuildIdentities</key><array>
                <dict><key>Info</key><dict><key>DeviceClass</key><string>n104ap</string><key>Variant</key><string>Customer Upgrade Install (IPSW)</string></dict>
                  <key>Manifest</key><dict><key>KernelCache</key><dict><key>Info</key><dict><key>Path</key><string>wrong</string></dict></dict></dict></dict>
                <dict><key>Info</key><dict><key>DeviceClass</key><string>N104AP</string><key>Variant</key><string>Customer Erase Install (IPSW)</string><key>Restore</key><true/></dict>
                  <key>Manifest</key><dict>
                    <key>KernelCache</key><dict><key>Info</key><dict><key>Path</key><string>kernelcache.release.iphone12</string></dict></dict>
                    <key>DeviceTree</key><dict><key>Info</key><dict><key>Path</key><string>Firmware/all_flash/DeviceTree.n104ap.im4p</string></dict></dict>
                    <key>RestoreRamDisk</key><dict><key>Info</key><dict><key>Path</key><string>099-1.dmg</string></dict></dict>
                    <key>Size</key><integer>42</integer>
                  </dict></dict>
              </array>
            </dict></plist>""",
        )
        assertEquals(
            VmConfig.Firmware("Restore/kernelcache.release.iphone12", "Restore/Firmware/all_flash/DeviceTree.n104ap.im4p",
                "Restore/Firmware/099-1.dmg.trustcache"),
            config.firmware,
        )
    }

    @Test fun blankAndWrittenQcow2() {
        val file = tmp.newFile("root.qcow2")
        RandomAccessFile(file, "rw").use { f ->
            f.write(byteArrayOf(0x51, 0x46, 0x49, 0xFB.toByte()))
            f.seek(36); f.writeInt(4)          // l1_size
            f.seek(40); f.writeLong(0x30000)   // l1_table_offset
            f.setLength(0x30000 + 32)
        }
        assertFalse(VmConfig.qcow2HasAllocatedData(file))
        RandomAccessFile(file, "rw").use { f -> f.seek(0x30000 + 8); f.writeLong(0x8000000000050000uL.toLong()) }
        assertTrue(VmConfig.qcow2HasAllocatedData(file))
    }
}
