package org.lighttouch.android

import android.content.Context
import android.system.Os
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle

/** The hardware a board stands for, as on the real device. */
internal data class BoardSpecs(val chip: String, val cpu: String, val gpu: String, val memory: String, val screen: String)

internal fun boardSpecs(board: String): BoardSpecs? = when (board) {
    "n45ap", "m68ap", "n82ap" -> BoardSpecs("Samsung S5L8900", "ARM1176JZF-S, 412 MHz", "PowerVR MBX Lite", "128 MB", "320 × 480")
    "n72ap" -> BoardSpecs("Samsung S5L8720", "ARM1176JZF-S, 533 MHz", "PowerVR MBX Lite", "128 MB", "320 × 480")
    "n18ap" -> BoardSpecs("Samsung S5L8922", "ARM Cortex-A8, 600 MHz", "PowerVR SGX535", "256 MB", "320 × 480")
    "n88ap" -> BoardSpecs("Samsung S5L8920", "ARM Cortex-A8, 600 MHz", "PowerVR SGX535", "256 MB", "320 × 480")
    "n81ap" -> BoardSpecs("Apple A4", "ARM Cortex-A8, 800 MHz", "PowerVR SGX535", "256 MB", "640 × 960 (Retina)")
    "n90ap" -> BoardSpecs("Apple A4", "ARM Cortex-A8, 800 MHz", "PowerVR SGX535", "512 MB", "640 × 960 (Retina)")
    "k48ap" -> BoardSpecs("Apple A4", "ARM Cortex-A8, 1 GHz", "PowerVR SGX535", "256 MB", "768 × 1024")
    "n42ap" -> BoardSpecs("Apple A6", "Apple Swift (2 cores), 1.3 GHz", "PowerVR SGX543MP3", "1 GB", "640 × 1136 (Retina)")
    else -> null
}

/**
 * Whether the space iOS frees comes back to the phone when the device stops: yaFTL (the S5L8920/8922 and A4 boards on
 * iOS 4 to 6) erases the flash blocks it no longer needs while iOS shuts down, and the engine frees each erased block in
 * the device's files; the 2G's and the iPhone 5's FTLs keep freed space until their flash fills (docs/STORAGE.md,
 * "Space iOS frees"). The desktop's lighttouch_model::details::freed_space_returns_at_stop is the same rule.
 */
internal fun freedSpaceReturnsAtStop(board: String, build: String): Boolean =
    board in setOf("n18ap", "n88ap", "n81ap", "n90ap", "k48ap") && build.takeWhile { it.isDigit() }.toIntOrNull() in 8..10

internal fun storageNote(board: String, build: String): String =
    if (freedSpaceReturnsAtStop(board, build)) "Space iOS frees is given back to your phone when you stop the device."
    else "Space iOS frees is not given back to your phone yet, so a device's changes only grow."

/** What a device's record holds on this phone, read from its files. */
internal data class DeviceFacts(
    val flashBytes: Long?, val created: String?, val identity: JSONObject?, val toolsSerial: Int?, val toolsVerdict: String?,
) {
    /** The space the record takes, which can take a while: a 2G's changes are one file per flash page. */
    data class Space(val baseBytes: Long, val changesBytes: Long, val logBytes: Long, val toolsBytes: Long) {
        val totalBytes get() = baseBytes + changesBytes + logBytes + toolsBytes
    }

    companion object {
        private val isLog: (File) -> Boolean = { it.name.startsWith("engine-") && it.name.endsWith(".log") || it.name == "guest-serial.log" }

        /** Space a file takes on the phone (its allocated blocks: the flash files are sparse). */
        private fun allocated(file: File): Long = try { Os.lstat(file.path).st_blocks * 512 } catch (_: Exception) { 0L }
        private fun tree(dir: File, filter: (File) -> Boolean = { true }): Long =
            dir.walkTopDown().filter { it.isFile && filter(it) }.sumOf { allocated(it) }

        /** The flash size the board's iOS sees: geometry.json (prepared from an IPSW) or a packed image's header. */
        private fun flash(base: File): Long? {
            File(base, "nand/geometry.json").takeIf { it.isFile }?.let {
                val g = JSONObject(it.readText())
                // The 2G's geometry names its chip selects; the others, buses and chip enables per bus.
                val chips = if (g.has("chip_selects")) g.getLong("chip_selects") else g.getLong("ce_per_bus") * g.getLong("buses")
                return g.getLong("page_bytes") * g.getLong("pages_per_block") * g.getLong("blocks_per_ce") * chips
            }
            val packed = File(base, "nand").takeIf { it.isFile } ?: return null
            val header = ByteArray(24)
            packed.inputStream().use { if (it.read(header) != 24) return null }
            if (String(header, 0, 8, Charsets.US_ASCII) != "ITNAND01") return null
            fun le(at: Int) = (0 until 4).fold(0L) { v, i -> v or ((header[at + i].toLong() and 0xff) shl (8 * i)) }
            return le(8) * le(16) * le(20) // page bytes × chip selects × pages per chip select
        }

        fun read(context: Context, id: String): DeviceFacts {
            val record = File(context.filesDir, "library/records/$id")
            val base = File(record, "base"); val state = File(record, "state")
            val lock = File(base, "device.lock.json").takeIf { it.isFile }?.let { runCatching { JSONObject(it.readText()) }.getOrNull() }
            val identity = File(base, "identity.json").takeIf { it.isFile }?.let { runCatching { JSONObject(it.readText()) }.getOrNull() }
            val verdict = File(state, "guest-verdicts").takeIf { it.isFile }?.readText()?.trim()
            return DeviceFacts(
                flashBytes = runCatching { flash(base) }.getOrNull(),
                created = lock?.optString("created")?.takeIf { it.isNotEmpty() },
                identity = identity,
                toolsSerial = verdict?.split(' ')?.getOrNull(1)?.toIntOrNull(),
                toolsVerdict = verdict?.substringBefore(' '),
            )
        }

        fun space(context: Context, id: String): Space {
            val record = File(context.filesDir, "library/records/$id"); val state = File(record, "state")
            return Space(tree(File(record, "base")), tree(state) { !isLog(it) }, tree(state, isLog), tree(File(record, "assets")))
        }
    }
}

internal fun formatBytes(bytes: Long): String = when {
    bytes >= 1L shl 30 -> "%.1f GB".format(bytes / (1L shl 30).toDouble())
    bytes >= 1L shl 20 -> "%.0f MB".format(bytes / (1L shl 20).toDouble())
    else -> "%.0f KB".format(bytes / 1024.0)
}

/** A page of what LightTouch knows about one device: its model and iOS, the hardware it stands for, the space it takes. */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun DeviceDetails(device: DeviceSummary, onClose: () -> Unit) {
    val context = LocalContext.current
    var facts by remember { mutableStateOf<DeviceFacts?>(null) }
    var space by remember { mutableStateOf<DeviceFacts.Space?>(null) }
    LaunchedEffect(device.id) { facts = withContext(Dispatchers.IO) { runCatching { DeviceFacts.read(context, device.id) }.getOrNull() } }
    LaunchedEffect(device.id) { space = withContext(Dispatchers.IO) { runCatching { DeviceFacts.space(context, device.id) }.getOrNull() } }
    BackHandler(onBack = onClose)
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }.testTag("device_details"), color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onClose, modifier = Modifier.testTag("details_back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                Text(device.name, style = MaterialTheme.typography.titleLarge, maxLines = 1)
            }
            SelectionContainer {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
                    val f = facts
                    val identity = f?.identity
                    Section("Device") {
                        Fact("Model", device.model + (identity?.optString("model-number")?.takeIf { it.isNotEmpty() }
                            ?.let { " ($it${identity.optString("region-info").let { r -> if (r.isNotEmpty()) " $r" else "" }})" } ?: ""))
                        Fact("iOS", device.target?.let { "${it.ios} (${device.build})" } ?: device.build)
                        Fact("Board", device.board)
                        identity?.optString("serial-number")?.takeIf { it.isNotEmpty() }?.let { Fact("Serial number", it) }
                        identity?.optString("udid")?.takeIf { it.isNotEmpty() }?.let { Fact("UDID", it) }
                        identity?.optString("wifi-mac")?.takeIf { it.isNotEmpty() }?.let { Fact("Wi-Fi address", it) }
                    }
                    boardSpecs(device.board)?.let { s ->
                        Section("Hardware", "As on the real device; LightTouch emulates it, so speed depends on your phone.") {
                            Fact("Chip", s.chip)
                            Fact("CPU", s.cpu)
                            Fact("GPU", s.gpu)
                            Fact("Memory", s.memory)
                            Fact("Screen", s.screen)
                            Fact("Motion sensors", listOfNotNull("accelerometer".takeIf { device.accelerometer }, "gyroscope".takeIf { device.gyroscope })
                                .joinToString(" and ").replaceFirstChar { it.uppercase() }.ifEmpty { "None" })
                        }
                    }
                    Section("Storage", storageNote(device.board, device.build)) {
                        f?.flashBytes?.let { Fact("Disk size", formatBytes(it) + " of flash") }
                        val u = space
                        if (u == null) Fact("On this phone", "Measuring…") else {
                            Fact("On this phone", formatBytes(u.totalBytes))
                            Fact("Prepared system", formatBytes(u.baseBytes))
                            Fact("Changes since then", formatBytes(u.changesBytes))
                            if (u.logBytes > 0) Fact("Engine logs", formatBytes(u.logBytes))
                        }
                    }
                    Section("LightTouch") {
                        f?.toolsSerial?.let { Fact("Guest tools", "Serial $it" + if (f.toolsVerdict == "good") "" else " (${f.toolsVerdict})") }
                        f?.created?.let { created ->
                            Fact("Prepared", runCatching { DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM)
                                .format(Instant.parse(created).atZone(ZoneId.systemDefault())) }.getOrDefault(created))
                        }
                        Fact("Record", device.id)
                        if (device.dirty) Fact("State", "Not shut down cleanly; it recovers at the next start")
                    }
                    Spacer(Modifier.height(24.dp))
                }
            }
        }
    }
}

@Composable
private fun Section(title: String, note: String? = null, content: @Composable ColumnScope.() -> Unit) {
    Text(title, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 20.dp, bottom = 8.dp))
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(vertical = 6.dp), content = content)
    note?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 6.dp)) }
}

@Composable
private fun Fact(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp), verticalAlignment = Alignment.Top) {
        Text(label, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.width(132.dp))
        Text(value, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
    }
}
