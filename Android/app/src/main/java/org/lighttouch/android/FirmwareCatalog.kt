package org.lighttouch.android

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import org.json.JSONObject
import java.io.File
import java.io.RandomAccessFile
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/** One firmware of the bundled catalog (Portable/firmware/firmware-catalog.json). */
internal data class CatalogFirmware(
    val id: String, val board: String, val productType: String, val version: String, val build: String,
    val released: String, val prerelease: String, val sha1: String, val bytes: Long, val urls: List<String>,
    /** The catalog's own label: available, experimental or untested (upstream's checks, not LightTouch's). */
    val status: String = "",
) {
    /** "3.1.3", or "4.2 beta 2" for a developer build. */
    val label get() = if (prerelease.isEmpty()) version else "$version $prerelease"
    val fileName get() = "${productType}_${version}_${build}_Restore.ipsw"
    val support get() = when {
        id in slowFirmwares -> Support.TESTED_SLOW
        id in testedFirmwares -> Support.TESTED
        status == "available" || status == "experimental" -> Support.EXPERIMENTAL
        else -> Support.UNTESTED
    }
    /**
     * Why the prepare screen shows the Jailbreak switch turned off and locked for this firmware, or null where it can
     * be turned on. firmwarekit refuses the same firmwares; the desktop's Firmware::jailbreak_unavailable says the same.
     */
    val jailbreakUnavailable: String? get() = when {
        (version.substringBefore('.').toIntOrNull() ?: 0) < 2 -> "Needs iPhone OS 2.0 or later"
        // Substrate's launchd hook (cynject from /etc/launchd.conf) hangs every boot after the first: docs/TODO.md.
        board == "n72ap" -> "Not on iPod touch 2G yet: it wouldn't start up again after its first restart"
        else -> null
    }
}

/** How far a firmware has been tried: LightTouch's own compatibility runs first, then the catalog's label. */
internal enum class Support(val label: String, val detail: String) {
    TESTED("Tested", "Checked on LightTouch for Android."),
    TESTED_SLOW("Tested, slow", "Checked on LightTouch for Android, but slower than the iPod touch 2G on iOS 3.1.3."),
    EXPERIMENTAL("Experimental", "Starts in Light Touch for Mac's checks; not yet checked on LightTouch for Android."),
    UNTESTED("Untested", "Nobody has tried these yet. They may not start."),
    ;
    val tested get() = this == TESTED || this == TESTED_SLOW
}

/** Firmwares that pass LightTouch's compatibility runs on Android (docs/COMPATIBILITY.md). */
internal val testedFirmwares = setOf(
    "n72ap-7E18", "n72ap-8C148",  // iPod touch 2G 3.1.3, 4.2.1
    "n18ap-8C148",  // iPod touch 3G 4.2.1
    "n81ap-10B500",  // iPod touch 4G 6.1.6
    "n88ap-9B206",  // iPhone 3GS 5.1.1
    "n90ap-8C148", "n90ap-8L1",  // iPhone 4 4.2.1, 4.3.5
)

/** The firmware to pick first: the fastest and most complete on phones. */
internal const val recommendedFirmware = "n72ap-7E18"

/** Tested firmwares that run noticeably slower than the iPod touch 2G on iOS 3.1.3, the one to pick first. */
internal val slowFirmwares = setOf("n88ap-9B206", "n90ap-8C148", "n90ap-8L1")

internal object FirmwareCatalog {
    fun load(context: Context): List<CatalogFirmware> =
        parse(context.assets.open("firmware-catalog.json").bufferedReader().use { it.readText() })

    fun parse(text: String): List<CatalogFirmware> {
        val entries = JSONObject(text).getJSONArray("entries")
        return (0 until entries.length()).map { entries.getJSONObject(it) }
            // "coming_soon" entries have no preparation recipe anywhere yet.
            .filter { it.has("product_type") && it.has("version") && it.optString("status") != "coming_soon" }.map { e ->
            val source = e.optJSONObject("source")
            val urls = source?.takeIf { it.optString("kind") == "ipsw" }?.let { s ->
                listOf(s.optString("url")) + (s.optJSONArray("mirrors")?.let { m -> (0 until m.length()).map { m.getJSONObject(it).optString("url") } } ?: emptyList())
            }?.filter { it.startsWith("https://") } ?: emptyList()
            val prerelease = e.optString("prerelease").let { kind -> if (kind.isEmpty()) "" else "$kind ${e.optInt("prerelease_number", 0).takeIf { it > 0 } ?: ""}".trim() }
            CatalogFirmware(e.getString("id"), e.getString("board"), e.getString("product_type"), e.getString("version"), e.getString("build"),
                e.optString("released"), prerelease, source?.optString("sha1") ?: "", source?.optLong("bytes") ?: 0L, urls, e.optString("status"))
        }
    }
}

/**
 * A SecureROM a device starts from: mask ROM in the chip, the same for every iOS version and never part of an IPSW.
 * The iPod touch 1G and the original iPhone share the S5L8900's, the iPod touch 2G has the S5L8720's (iBoot-240.4);
 * the later devices start from their prepared files and need none. securerom.fun and devos50's qemu-ios releases
 * publish dumps; a download or a chosen file must be these exact bytes (upstream LightTouchMac's bootrom files).
 */
internal enum class Bootrom(val label: String, val sha256: String, private val links: List<String>) {
    S5L8720("iBoot-240.4", "55f4d8ea2791ba51dd89934168f38f0fb21ce8762ff614c1e742407c0d3ca054", listOf(
        "https://securerom.fun/resources/APROM/SecureROM%20for%20s5l8720xsi%2C%20iBoot-240.4",
        "https://github.com/hekapooios/hekapooios.github.io/raw/master/resources/APROM/SecureROM%20for%20s5l8720xsi%2C%20iBoot-240.4",
        "https://github.com/devos50/qemu-ios/releases/download/n72ap_v1/bootrom_240_4")),
    S5L8900("S5L8900", "220027d6f8ea24603f8c5ad5e920ab553d85a9cd809d79c7e6f6f0eefdc2bc95", listOf(
        "https://github.com/devos50/qemu-ios/releases/download/n45ap_v1/bootrom_s5l8900")),
    ;
    /** Where downloads come from, for the prepare screen. */
    val sources get() = links.map { URL(it).host }.distinct().joinToString(" or ")
    fun matches(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) } == sha256

    /** Downloads the boot ROM to [target]; blocking, so call it off the main thread. */
    fun fetch(target: File) {
        var failure = ""
        for (link in links) {
            try {
                val connection = URL(link).openConnection() as HttpURLConnection
                connection.connectTimeout = 15_000; connection.readTimeout = 30_000
                val bytes = try {
                    check(connection.responseCode == 200) { "HTTP ${connection.responseCode}" }
                    connection.inputStream.use { it.readBytes() }
                } finally { connection.disconnect() }
                check(matches(bytes)) { "the file did not match" }
                val partial = File(target.path + ".partial"); partial.writeBytes(bytes)
                check(partial.renameTo(target)) { "could not save it" }
                return
            } catch (e: Exception) { failure = "${URL(link).host}: ${e.message}" }
        }
        error(failure)
    }

    companion object {
        /** The ROM [board] starts from, or null when it starts from its prepared files. */
        fun forBoard(board: String) = when (board) {
            "n72ap" -> S5L8720
            "n45ap", "m68ap" -> S5L8900
            else -> null
        }
    }
}

/** Whether a device needs its boot ROM to start (Bootrom.forBoard). */
internal fun needsBootrom(board: String) = Bootrom.forBoard(board) != null

/**
 * Downloads a stock IPSW: the link ipsw.me (api.ipsw.me/v4) lists for the build, then the catalog's
 * own links, then the Internet Archive's copy. The file must match the catalog's SHA-1 and size. A
 * partial download resumes where it stopped. Lives outside the activity so a download survives the
 * activity being recreated.
 */
internal object IpswDownload {
    var firmwareId by mutableStateOf("")
        private set
    var progress by mutableFloatStateOf(-1f)
        private set
    var running by mutableStateOf(false)
        private set
    var status by mutableStateOf("")
        private set
    @Volatile private var cancelled = false

    fun directory(context: Context) = File(context.filesDir, "ipsw")

    fun cancel() { cancelled = true }

    /** Starts downloading [firmware]; [onDone] runs on the download thread with the verified file, or null and an error. */
    fun start(context: Context, firmware: CatalogFirmware, onDone: (File?, String) -> Unit) {
        if (running) return
        running = true; cancelled = false; firmwareId = firmware.id; progress = -1f; status = "Looking up ${firmware.fileName}"
        val app = context.applicationContext
        DownloadService.hold(app, "Downloading iOS ${firmware.label}")
        Thread({
            val result = try { download(app, firmware) to "" } catch (e: Exception) { null to (e.message ?: "Download failed") }
            DownloadService.release(app)
            running = false
            status = if (result.first != null) "" else result.second
            onDone(result.first, result.second)
        }, "ipsw-download").start()
    }

    /** Asks ipsw.me for the build's link; it lists Apple's own servers, and lacks some builds (iPod touch 2G 3.x). */
    private fun ipswMe(firmware: CatalogFirmware): String? = try {
        val connection = URL("https://api.ipsw.me/v4/ipsw/${firmware.productType}/${firmware.build}").openConnection() as HttpURLConnection
        connection.connectTimeout = 10_000; connection.readTimeout = 10_000
        connection.setRequestProperty("Accept", "application/json")
        try {
            if (connection.responseCode != 200) null
            else JSONObject(connection.inputStream.bufferedReader().use { it.readText() }).let { info ->
                info.optString("url").takeIf { it.startsWith("https://") && (firmware.sha1.isEmpty() || info.optString("sha1sum").equals(firmware.sha1, true)) }
            }
        } finally { connection.disconnect() }
    } catch (_: Exception) { null }

    private fun download(context: Context, firmware: CatalogFirmware): File {
        val target = File(directory(context).apply { mkdirs() }, firmware.fileName)
        if (target.isFile && target.length() == firmware.bytes && sha1(target) == firmware.sha1) return target
        val partial = File(target.path + ".partial")
        // The Internet Archive's iOS Firmware Exhibit Collection keeps every IPSW under its Apple file name.
        val archive = "https://archive.org/download/iOS_Firmware_Exhibit_Collection/" + firmware.fileName.replace(",", "%2C")
        val links = (listOfNotNull(ipswMe(firmware)) + firmware.urls + archive).distinctBy { it.replace("%2C", ",") }
        require(links.isNotEmpty()) { "No download is known for iOS ${firmware.label}; choose an IPSW you have instead" }
        var failure = ""
        for (link in links) {
            check(!cancelled) { "Download cancelled" }
            try {
                fetch(link, partial, firmware.bytes)
                status = "Checking the download"
                val digest = sha1(partial)
                if (firmware.sha1.isNotEmpty() && digest != firmware.sha1) { partial.delete(); failure = "The download from ${URL(link).host} did not match"; continue }
                check(partial.renameTo(target)) { "Could not save the IPSW" }
                return target
            } catch (e: Exception) {
                if (cancelled) throw IllegalStateException("Download cancelled")
                failure = "${URL(link).host}: ${e.message}"
            }
        }
        error("Could not download iOS ${firmware.label} ($failure)")
    }

    private fun fetch(link: String, partial: File, expected: Long) {
        var have = if (partial.isFile) partial.length() else 0L
        if (expected > 0 && have > expected) { partial.delete(); have = 0 }
        val connection = URL(link).openConnection() as HttpURLConnection
        connection.connectTimeout = 15_000; connection.readTimeout = 30_000; connection.instanceFollowRedirects = true
        if (have > 0) connection.setRequestProperty("Range", "bytes=$have-")
        try {
            val code = connection.responseCode
            if (code == 416 && have == expected) return
            check(code == 200 || code == 206) { "HTTP $code" }
            if (code == 200) have = 0  // The server ignored the range; start over.
            val total = if (expected > 0) expected else have + connection.contentLengthLong
            status = "Downloading from ${URL(link).host}"
            RandomAccessFile(partial, "rw").use { out ->
                out.setLength(have); out.seek(have)
                connection.inputStream.use { input ->
                    val buffer = ByteArray(1 shl 16)
                    while (true) {
                        check(!cancelled) { "Download cancelled" }
                        val n = input.read(buffer); if (n < 0) break
                        out.write(buffer, 0, n); have += n
                        if (total > 0) progress = (have.toFloat() / total).coerceIn(0f, 1f)
                    }
                }
            }
            check(expected <= 0 || have == expected) { "the download stopped early" }
        } finally { connection.disconnect() }
    }

    fun sha1(file: File): String {
        val digest = MessageDigest.getInstance("SHA-1")
        file.inputStream().use { input -> val buffer = ByteArray(1 shl 20); while (true) { val n = input.read(buffer); if (n < 0) break; digest.update(buffer, 0, n) } }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }
}
