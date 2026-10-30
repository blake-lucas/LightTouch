package org.lighttouch.android

import android.os.Bundle
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

internal enum class PrepareStep { DEVICE, VERSION, FILES }

/** The prepare-from-IPSW flow's choices; the activity owns it so pickers, the download and the firmware worker can update it. */
internal class PrepareState {
    var catalog by mutableStateOf<List<CatalogFirmware>>(emptyList())
    /** Firmware ids this phone prepares itself: the whole catalog when the APK ships firmwarekit. */
    var step by mutableStateOf(PrepareStep.DEVICE)
    var productType by mutableStateOf("")
    var firmwareId by mutableStateOf("")
    /** A stock IPSW on this phone whose SHA-1 matched [ipswFor]'s catalog entry. */
    var ipswPath by mutableStateOf("")
    var ipswFor by mutableStateOf("")
    /** Copying and checking an IPSW the user chose; empty when idle. */
    var ipswChecking by mutableStateOf("")
    var ipswError by mutableStateOf("")
    /** A boot ROM whose bytes matched [romFor]'s, and where it came from. */
    var romPath by mutableStateOf("")
    var romFor by mutableStateOf<Bootrom?>(null)
    var romLabel by mutableStateOf("")
    var romChecking by mutableStateOf(false)
    var romError by mutableStateOf("")
    var preparation by mutableStateOf<Bundle?>(null)
    var importError by mutableStateOf("")
    /** Adding the prepared output as a device: the phase shown, and how far it is (negative while unknown). */
    var importPhase by mutableStateOf("")
    var importProgress by mutableFloatStateOf(-1f)
    var showBetas by mutableStateOf(false)
    /** afc2 and Cydia with Substrate: firmwarekit's `create --jailbreak`, downloaded while it prepares. */
    var jailbreak by mutableStateOf(false)
    val firmware get() = catalog.firstOrNull { it.id == firmwareId }
    val ipswReady get() = ipswPath.isNotEmpty() && ipswFor == firmwareId
    /** The boot ROM [firmware]'s device starts from; null when it starts from its prepared files. */
    val bootrom get() = firmware?.let { Bootrom.forBoard(it.board) }
    val romReady get() = firmware != null && (bootrom == null || (romPath.isNotEmpty() && romFor == bootrom))
    /** This phone builds [firmware] from its IPSW; the others are added as a copy prepared elsewhere. */
}

/** Order devices appear in: iPods, iPhones, then the iPad, each oldest first. */
private val deviceOrder = listOf("iPod1,1", "iPod2,1", "iPod3,1", "iPod4,1", "iPhone1,1", "iPhone1,2", "iPhone2,1", "iPhone3,1", "iPad1,1")

/** Version strings compared numerically: 3.1.10 after 3.1.3. */
private val versionOrder = Comparator<CatalogFirmware> { a, b ->
    val x = a.version.split('.').map { it.toIntOrNull() ?: 0 }; val y = b.version.split('.').map { it.toIntOrNull() ?: 0 }
    (0 until maxOf(x.size, y.size)).map { (x.getOrElse(it) { 0 }).compareTo(y.getOrElse(it) { 0 }) }.firstOrNull { it != 0 } ?: a.build.compareTo(b.build)
}

/**
 * What preparing a firmware costs on a recent phone (Poco F3). The iPod touch 2G only writes files (iOS 4 adds a
 * 12 s keybag boot); later systems also boot iOS twice (the data-protection keybag, then iOS's first-start setup):
 * iPod touch 4G 6.1.6 took 195 s.
 */
private data class PreparationCost(val time: String, val space: String)
private fun preparationCost(firmware: CatalogFirmware?) =
    if (firmware?.board == "n72ap") PreparationCost("about a minute", "1.5 GB") else PreparationCost("a few minutes", "5 GB")

/**
 * Builds a device on this phone from a stock IPSW: pick the device, then the iOS version, then get
 * the IPSW (downloaded, or one already on the phone) and the boot file, and prepare it.
 */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun PrepareScreen(state: PrepareState, busy: Boolean, preparing: Boolean, onDownload: () -> Unit, onChooseIpsw: () -> Unit,
                           onChooseRom: () -> Unit, onDownloadRom: () -> Unit, onPrepare: () -> Unit,
                           onCancel: () -> Unit, onRetryImport: () -> Unit, onClose: () -> Unit) {
    val run = state.preparation?.getString("state")
    val showProgress = preparing || run in listOf("complete", "failed", "cancelled")
    val back = {
        when {
            preparing -> {}
            showProgress -> onClose()
            state.step == PrepareStep.FILES -> state.step = PrepareStep.VERSION
            state.step == PrepareStep.VERSION -> state.step = PrepareStep.DEVICE
            // Backed all the way out: the next Prepare from an IPSW starts at the device list, not this choice.
            else -> { state.firmwareId = ""; onClose() }
        }
    }
    BackHandler { back() }
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }.testTag("prepare"), color = MaterialTheme.colorScheme.background) {
        // The setup wizard's backdrop, so moving between the two reads as one flow.
        Box(Modifier.fillMaxSize().background(Brush.verticalGradient(listOf(MaterialTheme.colorScheme.primary.copy(alpha = 0.10f), Color.Transparent))))
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().padding(4.dp), verticalAlignment = Alignment.CenterVertically) {
                if (!preparing) IconButton(onClick = back, modifier = Modifier.testTag("prepare_back")) {
                    if (showProgress || state.step == PrepareStep.DEVICE) Icon(Icons.Filled.Close, "Close") else Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back")
                } else Spacer(Modifier.size(48.dp))
            }
            Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 24.dp)) {
                when {
                    showProgress -> Progress(state, preparing, busy, onPrepare, onCancel, onRetryImport, onClose)
                    state.step == PrepareStep.DEVICE -> DeviceStep(state)
                    state.step == PrepareStep.VERSION -> VersionStep(state)
                    else -> FilesStep(state, busy, onDownload, onChooseIpsw, onChooseRom, onDownloadRom, onPrepare)
                }
                Spacer(Modifier.height(24.dp))
            }
        }
    }
}

@Composable
private fun Heading(title: String, detail: String) {
    Text(title, style = MaterialTheme.typography.headlineMedium)
    Spacer(Modifier.height(8.dp))
    Text(detail, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    Spacer(Modifier.height(20.dp))
}

@Composable
private fun SectionLabel(text: String) {
    Text(text, Modifier.padding(top = 16.dp, bottom = 6.dp), style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
}

@Composable
private fun ChoiceRow(icon: ImageVector?, title: String, detail: String, enabled: Boolean, tag: String, badge: Support? = null, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().padding(vertical = 4.dp).clip(RoundedCornerShape(16.dp)).background(MaterialTheme.colorScheme.surfaceContainer)
        .clickable(enabled = enabled, onClick = onClick).alpha(if (enabled) 1f else 0.45f).padding(16.dp).testTag(tag),
        verticalAlignment = Alignment.CenterVertically) {
        if (icon != null) { Icon(icon, null, Modifier.size(26.dp), tint = MaterialTheme.colorScheme.primary); Spacer(Modifier.width(16.dp)) }
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (badge != null) SupportBadge(badge)
    }
}

@Composable
private fun SupportBadge(support: Support) {
    val color = when (support) {
        Support.TESTED -> MaterialTheme.colorScheme.primary
        Support.TESTED_SLOW -> MaterialTheme.colorScheme.tertiary
        Support.EXPERIMENTAL -> LtColors.Warning
        Support.UNTESTED -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Text(support.label, Modifier.padding(start = 12.dp).clip(RoundedCornerShape(8.dp)).background(color.copy(alpha = 0.14f))
        .padding(horizontal = 8.dp, vertical = 3.dp), style = MaterialTheme.typography.labelSmall, color = color)
}

@Composable
private fun DeviceStep(state: PrepareState) {
    Heading("Choose a device", "Pick the device to emulate. You'll choose its iOS version next.")
    val devices = state.catalog.groupBy { it.productType }.entries.sortedBy { deviceOrder.indexOf(it.key).let { i -> if (i < 0) 99 else i } }
    val (tested, others) = devices.partition { (_, firmwares) -> firmwares.any { it.support.tested } }
    for ((product, firmwares) in tested.sortedBy { (_, firmwares) -> firmwares.minOf { it.support } }) {
        val versions = firmwares.filter { it.support.tested }.sortedWith(versionOrder).joinToString(", ") { "iOS ${it.label}" }
        val more = firmwares.count { !it.support.tested && it.prerelease.isEmpty() }
        DeviceRow(state, product, "Tested: $versions" + if (more > 0) " · $more more to try" else "", firmwares.minOf { it.support })
    }
    if (others.isNotEmpty()) {
        SectionLabel("Not tested on LightTouch yet")
        Text("These may not start, or may not work fully. Try them and see.", Modifier.padding(bottom = 6.dp),
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        for ((product, firmwares) in others) {
            val sorted = firmwares.filter { it.prerelease.isEmpty() }.sortedWith(versionOrder).ifEmpty { firmwares.sortedWith(versionOrder) }
            val range = if (sorted.size == 1) "iOS ${sorted.first().label}" else "iOS ${sorted.first().version} to ${sorted.last().version}"
            DeviceRow(state, product, range, firmwares.minOf { it.support })
        }
    }
    if (state.catalog.isEmpty()) Text("Loading the firmware list…", style = MaterialTheme.typography.bodyMedium)
}

@Composable
private fun DeviceRow(state: PrepareState, product: String, detail: String, support: Support) {
    ChoiceRow(if (product.startsWith("iPad")) LtIcons.Tablet else LtIcons.Phone, shortProductName(product), detail, true, "prepare_device_$product", support) {
        state.productType = product
        if (state.firmware?.productType != product) state.firmwareId = ""
        state.step = PrepareStep.VERSION
    }
}

@Composable
private fun VersionStep(state: PrepareState) {
    val name = shortProductName(state.productType)
    Heading("Choose iOS for $name", "Tested versions are checked on LightTouch; the others are yours to try.")
    val firmwares = state.catalog.filter { it.productType == state.productType }.sortedWith(versionOrder.reversed())
    val betas = firmwares.count { it.prerelease.isNotEmpty() }
    val shown = firmwares.filter { it.prerelease.isEmpty() || state.showBetas }
    for (support in Support.entries) {
        val group = shown.filter { it.support == support }
        if (group.isEmpty()) continue
        SectionLabel(support.label)
        Text(support.detail, Modifier.padding(bottom = 6.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        for (firmware in group.sortedBy { it.id != recommendedFirmware }) {
            val detail = (if (firmware.id == recommendedFirmware) "Recommended · " else "") + released(firmware)
            ChoiceRow(null, "iOS ${firmware.label}", detail, true, "prepare_version_${firmware.id}") {
                state.firmwareId = firmware.id; state.step = PrepareStep.FILES
            }
        }
    }
    if (betas > 0) TextButton(onClick = { state.showBetas = !state.showBetas }, modifier = Modifier.padding(top = 8.dp).testTag("prepare_show_betas")) {
        Text(if (state.showBetas) "Hide beta versions" else "Show $betas beta versions")
    }
}

private fun released(firmware: CatalogFirmware): String {
    val date = firmware.released.split('-').takeIf { it.size >= 2 }?.let { (year, month) ->
        val months = listOf("January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December")
        months.getOrNull((month.toIntOrNull() ?: 0) - 1)?.let { "$it $year" }
    }
    return listOfNotNull(date?.let { "Released $it" }, "build ${firmware.build}").joinToString(" · ")
}

@Composable
private fun ColumnScope.FilesStep(state: PrepareState, busy: Boolean, onDownload: () -> Unit, onChooseIpsw: () -> Unit, onChooseRom: () -> Unit,
                                  onDownloadRom: () -> Unit, onPrepare: () -> Unit) {
    val firmware = state.firmware ?: return
    val name = shortProductName(firmware.productType)
    Heading("$name, iOS ${firmware.label}", "LightTouch needs Apple's stock IPSW for this version" +
        (if (needsBootrom(firmware.board)) ", and the device's boot file." else "."))
    // IPSW
    Card(icon = LtIcons.Layers, title = "iOS firmware (IPSW)", done = state.ipswReady,
        detail = if (state.ipswReady) "${java.io.File(state.ipswPath).name} · verified" else "${firmware.fileName} · ${firmware.bytes / 1_000_000} MB") {
        val downloading = IpswDownload.running && IpswDownload.firmwareId == firmware.id
        when {
            state.ipswReady -> TextButton(onClick = onChooseIpsw, enabled = !busy) { Text("Use a different file") }
            downloading -> {
                Text(IpswDownload.status, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.height(8.dp))
                if (IpswDownload.progress >= 0f) LinearProgressIndicator(progress = { IpswDownload.progress }, Modifier.fillMaxWidth())
                else LinearProgressIndicator(Modifier.fillMaxWidth())
                TextButton(onClick = { IpswDownload.cancel() }, modifier = Modifier.testTag("prepare_cancel_download")) { Text("Cancel download") }
            }
            state.ipswChecking.isNotEmpty() -> {
                Text(state.ipswChecking, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.height(8.dp)); LinearProgressIndicator(Modifier.fillMaxWidth())
            }
            else -> {
                val error = state.ipswError.ifEmpty { IpswDownload.status.takeIf { IpswDownload.firmwareId == firmware.id } ?: "" }
                if (error.isNotEmpty()) Text(error, Modifier.padding(bottom = 8.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = onDownload, enabled = !busy && !IpswDownload.running, modifier = Modifier.testTag("prepare_download")) {
                        Icon(LtIcons.Download, null, Modifier.size(18.dp)); Spacer(Modifier.width(8.dp)); Text("Download")
                    }
                    OutlinedButton(onClick = onChooseIpsw, enabled = !busy, modifier = Modifier.testTag("prepare_choose_ipsw")) { Text("Choose a file") }
                }
                Text("Downloads come from Apple's servers when ipsw.me lists them, otherwise from the Internet Archive.",
                    Modifier.padding(top = 8.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
    // Boot file: which one the device starts from, always shown.
    val rom = state.bootrom
    if (rom == null) Card(icon = LtIcons.Chip, title = "Boot file", done = true,
        detail = "None needed. The $name starts from the files preparation makes.") {}
    else Card(icon = LtIcons.Chip, title = "Boot file · ${rom.label} boot ROM", done = state.romReady,
        detail = if (state.romReady) state.romLabel else "64 KB, part of the chip rather than the IPSW. The same for every iOS version.") {
        when {
            state.romReady -> TextButton(onClick = onChooseRom, enabled = !busy) { Text("Use a different file") }
            state.romChecking -> LinearProgressIndicator(Modifier.fillMaxWidth())
            else -> {
                if (state.romError.isNotEmpty()) Text(state.romError, Modifier.padding(bottom = 8.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = onDownloadRom, enabled = !busy, modifier = Modifier.testTag("prepare_download_rom")) {
                        Icon(LtIcons.Download, null, Modifier.size(18.dp)); Spacer(Modifier.width(8.dp)); Text("Download")
                    }
                    OutlinedButton(onClick = onChooseRom, enabled = !busy, modifier = Modifier.testTag("prepare_choose_rom")) { Text("Choose a file") }
                }
                Text("Downloads come from ${rom.sources}, and must match the ${rom.label} boot ROM exactly.", Modifier.padding(top = 8.dp),
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
    Spacer(Modifier.height(12.dp))
    // afc2 and Cydia with Substrate, from Legacy iOS Kit's bootstrap (firmwarekit downloads it while it prepares). Off
    // and locked, saying why, where the firmware can't be jailbroken.
    val jailbreakUnavailable = firmware.jailbreakUnavailable
    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp).clip(RoundedCornerShape(16.dp)).background(MaterialTheme.colorScheme.surfaceContainer)) {
        SwitchRow(LtIcons.Bag, "Jailbreak", jailbreakUnavailable ?: "Cydia and afc2, installed during preparation",
            state.jailbreak && jailbreakUnavailable == null, tag = "prepare_jailbreak",
            enabled = jailbreakUnavailable == null) { if (!busy) state.jailbreak = it }
    }
    val cost = preparationCost(firmware)
    Text("Preparing runs on this phone, takes ${cost.time} and needs about ${cost.space} free. LightTouch's guest tools are added for you.",
        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    Spacer(Modifier.height(20.dp))
    Button(onClick = onPrepare, enabled = !busy && state.ipswReady && state.romReady && state.ipswChecking.isEmpty(),
        modifier = Modifier.align(Alignment.CenterHorizontally).height(52.dp).testTag("start_preparation"), contentPadding = PaddingValues(horizontal = 32.dp)) {
        Text("Prepare device", style = MaterialTheme.typography.titleMedium)
    }
}

@Composable
private fun Card(icon: ImageVector, title: String, done: Boolean, detail: String, content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp).clip(RoundedCornerShape(16.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(icon, null, Modifier.size(24.dp), tint = if (done) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(title, style = MaterialTheme.typography.titleSmall)
                Text(detail, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (done) Icon(Icons.Filled.CheckCircle, "Ready", tint = MaterialTheme.colorScheme.primary)
        }
        Spacer(Modifier.height(8.dp))
        content()
    }
}

@Composable
private fun Progress(state: PrepareState, preparing: Boolean, busy: Boolean, onPrepare: () -> Unit, onCancel: () -> Unit, onRetryImport: () -> Unit, onClose: () -> Unit) {
    val run = state.preparation
    val phase = run?.getString("state") ?: "starting"
    val firmware = state.firmware
    val title = firmware?.let { "${shortProductName(it.productType)}, iOS ${it.label}" } ?: "Your device"
    Column(Modifier.fillMaxWidth().padding(top = 48.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        when {
            preparing -> {
                Text("Preparing $title", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
                Spacer(Modifier.height(8.dp))
                // firmwarekit's detail is "What the step is doing — 45 s": the step centred, its time on a line of its own.
                val (stage, elapsed) = (run?.getString("stage") ?: "").split(" — ", limit = 2).let { it[0] to it.getOrElse(1) { "" } }
                Text(if (phase == "cancelling") "Cancelling…" else stage.ifEmpty { "Starting" }, Modifier.testTag("preparation_status"),
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
                if (elapsed.isNotEmpty() && phase != "cancelling")
                    Text(elapsed, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.height(24.dp))
                val done = run?.getLong("done") ?: 0L; val total = run?.getLong("total") ?: 0L
                if (total > 0 && done > 0) LinearProgressIndicator(progress = { (done.toFloat() / total).coerceIn(0f, 1f) }, Modifier.fillMaxWidth())
                else LinearProgressIndicator(Modifier.fillMaxWidth())
                Spacer(Modifier.height(12.dp))
                Text("This takes ${preparationCost(firmware).time}. You can leave LightTouch; preparation continues in the background.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
                Spacer(Modifier.height(24.dp))
                OutlinedButton(onClick = onCancel, enabled = phase != "cancelling", modifier = Modifier.testTag("cancel_preparation")) { Text("Cancel") }
            }
            phase == "complete" && state.importError.isNotEmpty() -> {
                Text("$title is prepared", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
                Spacer(Modifier.height(8.dp))
                Text("It could not be added as a device: ${state.importError}", style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                Spacer(Modifier.height(24.dp))
                Button(onClick = onRetryImport, enabled = !busy) { Text("Try adding it again") }
            }
            phase == "complete" -> {
                Text("Adding $title", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
                Spacer(Modifier.height(8.dp))
                val fraction = state.importProgress
                Text(state.importPhase.ifEmpty { "Setting up the iOS system…" } + if (fraction >= 0) " ${(fraction * 100).toInt()}%" else "",
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.height(24.dp))
                if (fraction >= 0) LinearProgressIndicator(progress = { fraction.coerceIn(0f, 1f) }, Modifier.fillMaxWidth())
                else LinearProgressIndicator(Modifier.fillMaxWidth())
            }
            else -> {
                Text(if (phase == "cancelled") "Preparation cancelled" else "Preparation failed", style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(8.dp))
                run?.getString("error")?.takeIf { it.isNotEmpty() && phase != "cancelled" }?.let {
                    Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                }
                Spacer(Modifier.height(24.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { state.preparation = null }) { Text("Back") }
                    Button(onClick = onPrepare, enabled = !busy && state.ipswReady && state.romReady) { Text("Try again") }
                }
            }
        }
    }
}
