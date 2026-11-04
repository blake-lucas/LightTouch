package org.lighttouch.inferno

import java.io.File
import java.io.RandomAccessFile

/**
 * What the user can change before a start. Kept in SharedPreferences by
 * [Prefs]; the launcher turns it into a command line and hands that to the
 * :vm process, so both processes never have to agree on a settings file.
 */
data class VmSettings(
    /** Guest cores including the SEP's: 4 is three AP cores and the SEP. */
    val cores: Int = 4,
    val memoryGb: Int = 3,
    /**
     * Half the panel at scale 1 instead of 828×1792 at 2: the same interface
     * over a quarter of the pixels, which the phone then scales back up.
     */
    val halfResolution: Boolean = true,
    val network: Boolean = true,
    val tbSizeMb: Int = 128,
    /**
     * Map the translation buffer twice, writable and executable, instead of
     * once with both: for phones whose kernel refuses memory that is both.
     */
    val splitWx: Boolean = false,
    /** Empty means [VmConfig.DEFAULT_BOOT_ARGS]. */
    val bootArgs: String = "",
    val volumeKeys: Boolean = true,
    val haptics: Boolean = true,
    val showFps: Boolean = false,
) {
    val displayWidth get() = if (halfResolution) 414 else 828
    val displayHeight get() = if (halfResolution) 896 else 1792
    val displayScale get() = if (halfResolution) 1 else 2
}

/**
 * The emulator's command line and what is missing for it — a port of
 * Inferno-iOS's `VMConfig.swift`, with the same layout on disk:
 *
 *     <root>/InfernoData/…            the desktop kit's data directory
 *     <root>/AppleSEPROM-Cebu-B1      the SEP ROM
 *
 * `root` is the app's external files directory
 * (Android/data/org.lighttouch.inferno/files), a real path the engine can open
 * and one a computer can copy into over USB.
 */
class VmConfig(val root: File) {
    val dataDirectory = File(root, "InfernoData")
    val sepRom = File(root, "AppleSEPROM-Cebu-B1")
    val guestConsoleLog = File(root, "guest-console.log")
    val logDirectory = File(root, "logs")
    val engineLog = File(logDirectory, "engine.log")
    val transferImage = File(dataDirectory, "xfer")

    data class RootImage(val file: File, val format: String)

    data class Firmware(val kernel: String, val deviceTree: String, val trustcache: String)

    data class Requirement(val label: String, val file: File) {
        val present get() = usable(file)
    }

    /** The device disk: the qcow2 conversion if there is one, else the raw file. */
    val rootImage: RootImage?
        get() {
            val qcow = File(dataDirectory, "root.qcow2")
            if (qcow.isFile) return RootImage(qcow, "qcow2")
            val raw = File(dataDirectory, "root")
            if (raw.isFile) return RootImage(raw, "raw")
            return null
        }

    /**
     * Whether the disk holds a system at all. A blank disk boots into nothing
     * and looks exactly like a hang, so the first bytes are worth a look.
     */
    val systemInstalled: Boolean
        get() {
            val image = rootImage ?: return false
            return if (image.format == "qcow2") qcow2HasAllocatedData(image.file) else rawHasData(image.file)
        }

    /** What the machine loads, out of the IPSW's own manifest when there is one. */
    val firmware: Firmware
        get() {
            val identity = eraseIdentity() ?: return FALLBACK_FIRMWARE
            val ramdisk = identity.path("RestoreRamDisk")
            val trustcache = identity.path("RestoreTrustCache") ?: ramdisk?.let { "Firmware/$it.trustcache" }
            val kernel = identity.path("KernelCache")
            val tree = identity.path("DeviceTree")
            if (kernel == null || tree == null || trustcache == null) return FALLBACK_FIRMWARE
            val resolved = Firmware("Restore/$kernel", "Restore/$tree", "Restore/$trustcache")
            // A manifest naming files nobody copied over is worse than none.
            val present = listOf(resolved.kernel, resolved.deviceTree, resolved.trustcache)
                .all { usable(File(dataDirectory, it)) }
            return if (present) resolved else FALLBACK_FIRMWARE
        }

    /** Everything the machine needs, in the order a person should fix it. */
    fun requirements(): List<Requirement> {
        val fw = firmware
        val list = mutableListOf<Requirement>()
        list += Requirement("Диск устройства (root.qcow2 или root)", rootImage?.file ?: File(dataDirectory, "root.qcow2"))
        list += REQUIRED_FILES.map { (label, path) -> Requirement(label, File(root, path)) }
        list += Requirement("Kernelcache", File(dataDirectory, fw.kernel))
        list += Requirement("Device tree", File(dataDirectory, fw.deviceTree))
        list += Requirement("TrustCache", File(dataDirectory, fw.trustcache))
        return list
    }

    fun missing(): List<String> = requirements().filterNot { it.present }.map { it.label }

    fun bootArguments(settings: VmSettings) = settings.bootArgs.trim().ifEmpty { DEFAULT_BOOT_ARGS }

    /**
     * The command line, argument for argument the one Inferno-iOS builds for an
     * ordinary start (no restore, no VirtualHere), with the display read in
     * process and no sound.
     *
     * @param qemuData where the engine's data files were unpacked (-L)
     */
    fun arguments(settings: VmSettings, qemuData: File): List<String> {
        val data = dataDirectory.path
        val fw = firmware
        val machine = listOf(
            "t8030",
            // The guest's USB port dials this socket; the emulator's own CDC-NCM
            // device listens there. A bare name: the engine runs chdir'd into
            // the socket's folder, because sun_path holds only 108 bytes.
            "usb-uplink-type=inferno", "usb-uplink-addr=unix:$USB_SOCKET",
            "trustcache=$data/${fw.trustcache}",
            "ticket=$data/root_ticket.der",
            "sep-fw=$data/sep-firmware.n104.RELEASE.new.img4",
            "sep-rom=${sepRom.path}",
            "kaslr-off=true",
            // Said every time: an NVRAM left with auto-boot=false by an
            // unfinished restore would otherwise send the machine to recovery.
            "boot-mode=exit_recovery",
            "disp-width=${settings.displayWidth}",
            "disp-height=${settings.displayHeight}",
            "disp-scale=${settings.displayScale}",
        ).joinToString(",")

        // Multi-threaded TCG, never single: the SEP and the AP cores must move
        // together or the SEP panics setting up its key store.
        val accel = "tcg,thread=multi,tb-size=${settings.tbSizeMb}" + if (settings.splitWx) ",split-wx=on" else ""

        val argv = mutableListOf(
            "qemu-system-aarch64",
            "-L", qemuData.path,
            "-accel", accel,
            "-M", machine,
            "-kernel", "$data/${fw.kernel}",
            "-dtb", "$data/${fw.deviceTree}",
            "-append", bootArguments(settings),
            "-smp", settings.cores.toString(),
            "-m", "${settings.memoryGb}G",
            // The console goes to a file from its first byte; the socket is there
            // for anyone who wants to talk to it (adb forward tcp:4555 tcp:4555).
            "-chardev", "socket,id=serial0,host=127.0.0.1,port=$SERIAL_PORT,server=on,wait=off," +
                "logfile=${guestConsoleLog.path},logappend=on",
            "-serial", "chardev:serial0",
            "-qmp", "tcp:127.0.0.1:$QMP_PORT,server,nowait",
            "-drive", "file=$data/sep_nvram,if=pflash,format=raw",
            "-drive", "file=$data/sep_ssc,if=pflash,format=raw",
            // Silence asked for by name, in the global's long form: the driver is
            // called apple.mca, and the short form would split at its dot.
            "-audiodev", "none,id=quiet",
            "-global", "driver=apple.mca,property=audiodev,value=quiet",
            "-display", "none",
        )

        rootImage?.let { image ->
            argv += listOf("-drive", "file=${image.file.path},format=${image.format},if=none,id=root")
            argv += listOf("-device", "nvme-ns,drive=root,bus=nvme-bus.0,nsid=1,nstype=1,$BLOCKS")
        }
        for ((file, nsid, nstype) in NAMESPACES) {
            argv += listOf("-drive", "file=$data/$file,format=raw,if=none,id=$file")
            argv += listOf("-device", "nvme-ns,drive=$file,bus=nvme-bus.0,nsid=$nsid,nstype=$nstype,$BLOCKS")
        }
        // The scratch namespace both sides reach, only when the file exists.
        // cache=none, or the guest reads what the page cache used to hold.
        if (transferImage.isFile) {
            argv += listOf("-drive", "file=${transferImage.path},format=raw,if=none,id=xfer,cache=none")
            argv += listOf("-device", "nvme-ns,drive=xfer,bus=nvme-bus.0,nsid=8,nstype=2,$BLOCKS")
        }
        if (settings.network) {
            argv += listOf("-netdev", "user,id=net0")
            argv += listOf("-device", "apple-ncm-host,netdev=net0,conn-addr=$USB_SOCKET")
        }
        // nvram is its own device type, not a plain namespace.
        argv += listOf("-drive", "file=$data/nvram,if=none,format=raw,id=nvram")
        argv += listOf("-device", "apple-nvram,drive=nvram,bus=nvme-bus.0,nsid=5,nstype=5,id=nvram,$BLOCKS")
        return argv
    }

    private class Identity(val manifest: Map<*, *>) {
        fun path(component: String): String? =
            ((manifest[component] as? Map<*, *>)?.get("Info") as? Map<*, *>)?.get("Path") as? String
    }

    /** The iPhone 11's erase identity out of Restore/BuildManifest.plist. */
    private fun eraseIdentity(): Identity? {
        val file = File(dataDirectory, "Restore/BuildManifest.plist")
        if (!file.isFile) return null
        val stamp = file.lastModified() to file.length()
        cachedManifest?.let { (s, id) -> if (s == stamp) return id }
        val root = Plist.parse(file) as? Map<*, *> ?: return null
        val identities = root["BuildIdentities"] as? List<*> ?: return null
        val found = identities.asSequence().mapNotNull { it as? Map<*, *> }.firstOrNull { identity ->
            val info = identity["Info"] as? Map<*, *> ?: return@firstOrNull false
            (info["DeviceClass"] as? String)?.lowercase() == "n104ap" &&
                (info["Variant"] as? String)?.contains("Erase") == true &&
                identity["Manifest"] is Map<*, *>
        }?.let { Identity(it["Manifest"] as Map<*, *>) } ?: return null
        cachedManifest = stamp to found
        return found
    }

    private var cachedManifest: Pair<Pair<Long, Long>, Identity>? = null

    companion object {
        const val SERIAL_PORT = 4555
        const val QMP_PORT = 4556
        const val USB_SOCKET = "inferno-usb.sock"
        private const val BLOCKS = "logical_block_size=4096,physical_block_size=4096"

        /**
         * As Inferno-iOS: no mtxspin=-1, which only makes waiting cores spin
         * while a lock's owner is descheduled — a losing bet under emulation.
         */
        const val DEFAULT_BOOT_ARGS =
            "tlto_us=-1 agm-genuine=1 agm-authentic=1 agm-trusted=1 serial=3 wdt=-1 launchd_unsecure_cache=1 -vm_compressor_wk_sw"

        /** The names iOS 14.0 beta 5 uses, for kits without a manifest. */
        val FALLBACK_FIRMWARE = Firmware(
            kernel = "Restore/kernelcache.release.iphone12b",
            deviceTree = "Restore/Firmware/all_flash/DeviceTree.n104ap.im4p",
            trustcache = "Restore/Firmware/038-44135-124.dmg.trustcache",
        )

        val REQUIRED_FILES = listOf(
            "Прошивка NVMe" to "InfernoData/firmware",
            "syscfg" to "InfernoData/syscfg",
            "ctrl_bits" to "InfernoData/ctrl_bits",
            "nvram" to "InfernoData/nvram",
            "effaceable" to "InfernoData/effaceable",
            "panic_log" to "InfernoData/panic_log",
            "SEP nvram" to "InfernoData/sep_nvram",
            "SEP ssc" to "InfernoData/sep_ssc",
            "Тикет" to "InfernoData/root_ticket.der",
            "Прошивка SEP" to "InfernoData/sep-firmware.n104.RELEASE.new.img4",
            "SEP ROM" to "AppleSEPROM-Cebu-B1",
        )

        /** The NVMe namespaces besides root and nvram, in order. */
        val NAMESPACES = listOf(
            Triple("firmware", 2, 2),
            Triple("syscfg", 3, 3),
            Triple("ctrl_bits", 4, 4),
            Triple("effaceable", 6, 6),
            Triple("panic_log", 7, 8),
        )

        /**
         * A file, not a folder, and not empty: an unpacked archive can leave a
         * folder called `firmware`, and QEMU then calls exit(1) from inside
         * qemu_init — which takes the whole process with it.
         */
        fun usable(file: File) = file.isFile && file.length() > 0

        private fun rawHasData(file: File): Boolean = runCatching {
            RandomAccessFile(file, "r").use { f ->
                val head = ByteArray(minOf(64 * 1024L, f.length()).toInt())
                f.readFully(head)
                head.any { it.toInt() != 0 }
            }
        }.getOrDefault(false)

        /**
         * Whether a qcow2 image was ever written to: a blank one has an L1 table
         * of nothing but zeros. Anything unreadable counts as written, so a
         * check that cannot be made never blocks a start.
         */
        fun qcow2HasAllocatedData(file: File): Boolean = runCatching {
            RandomAccessFile(file, "r").use { f ->
                val header = ByteArray(48)
                if (f.length() < 48) return true
                f.readFully(header)
                if (header[0] != 0x51.toByte() || header[1] != 0x46.toByte() ||
                    header[2] != 0x49.toByte() || header[3] != 0xFB.toByte()
                ) return true
                fun be32(at: Int) = (0 until 4).fold(0L) { acc, i -> (acc shl 8) or (header[at + i].toLong() and 0xFF) }
                fun be64(at: Int) = (0 until 8).fold(0L) { acc, i -> (acc shl 8) or (header[at + i].toLong() and 0xFF) }
                val l1Size = be32(36)
                val l1Offset = be64(40)
                if (l1Size <= 0) return false
                f.seek(l1Offset)
                val table = ByteArray(minOf(l1Size * 8, 16L * 1024 * 1024).toInt())
                val n = f.read(table)
                if (n <= 0) return true
                (0 until n).any { table[it].toInt() != 0 }
            }
        }.getOrDefault(true)
    }
}
