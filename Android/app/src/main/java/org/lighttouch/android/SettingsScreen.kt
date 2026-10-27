package org.lighttouch.android

import android.content.Intent
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.KeyboardArrowRight
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.dp

@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun SettingsScreen(host: GuestHost, onBack: () -> Unit) {
    var networkOpen by androidx.compose.runtime.saveable.rememberSaveable { mutableStateOf(false) }
    if (networkOpen) { NetworkSettingsScreen(host) { networkOpen = false }; return }
    BackHandler(onBack = onBack)
    val s = host.settings
    val context = LocalContext.current
    var headphonesInfo by remember { mutableStateOf(false) }
    if (headphonesInfo) AlertDialog(onDismissRequest = { headphonesInfo = false }, icon = { Icon(LtIcons.Headphones, null) },
        title = { Text("Headphones or speaker", style = MaterialTheme.typography.titleMedium) },
        text = { Text("With headphones, iOS plays games and music in stereo, unchanged.\n\n" +
            "On the speaker, iOS reshapes the sound for the device's small built-in speaker: mono, with the bass cut and a limiter to protect it. " +
            "That processing runs on the emulated CPU, so games run a little slower, and the sound is closer to an old iPod's speaker than to your phone's.\n\n" +
            "Headphones also turn off the speaker's protection limiter. Some games count on it to keep loud sounds in check and can sound distorted without it; " +
            "if a game crackles or clips, switch this off and restart the device.") },
        confirmButton = { TextButton(onClick = { headphonesInfo = false }) { Text("OK") } })
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }, color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack, modifier = Modifier.testTag("settings_back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                Text("Settings", style = MaterialTheme.typography.titleLarge)
            }
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).testTag("settings")) {
                SettingsGroup("Controls") {
                    SwitchRow(LtIcons.Tilt, "Tilt controls", "Your phone's motion sensors move the device's accelerometer", s.tilt, "tilt_toggle") { s.tilt = it; host.applySettings() }
                    SwitchRow(LtIcons.VolumeUp, "Phone volume keys control the device", "Off, they set your phone's volume as usual; the device's own volume buttons are on screen", s.volumeKeys) { s.volumeKeys = it }
                    SwitchRow(LtIcons.Vibrate, "Haptic feedback", "A tap when you press Home, Sleep/Wake or volume", s.haptics) { s.haptics = it }
                }
                SettingsGroup("Audio") {
                    SwitchRow(LtIcons.Headphones, "Headphones plugged in", "Stereo sound, as through the headphone jack; off plays through the device's speaker. Applies the next time the device starts",
                        s.headphones, "headphones_toggle", info = { headphonesInfo = true }) { s.headphones = it }
                }
                SettingsGroup("Connection") {
                    ActionRow(LtIcons.Network, "Network", s.network.summary, tag = "open_network") { networkOpen = true }
                }
                SettingsGroup("Display") {
                    ChoiceRow(LtIcons.Palette, "Theme", ThemeMode.entries, s.theme, { it.label }) { s.theme = it }
                    ChoiceRow(LtIcons.Layers, "Screen scaling", ScreenScaling.entries, s.scaling, { it.label },
                        if (s.scaling == ScreenScaling.SMOOTH) "Filtered by the display hardware" else "Crisp pixels, then filtered to fit") { s.scaling = it; host.applySettings() }
                    SwitchRow(LtIcons.Play, "Unpause when you return", "iOS pauses while LightTouch is in the background, unless apps are installing. On, it carries on when you come back; off, it waits for you to tap Resume", s.autoResume) { s.autoResume = it }
                }
                SettingsGroup("Performance overlay") {
                    ChoiceRow(LtIcons.Speed, "Detail", OverlayLevel.entries, s.overlay, { it.label }, tag = "overlay_level") { s.overlay = it }
                    if (s.overlay != OverlayLevel.OFF) {
                        OverlayPreview(host)
                        ChoiceRow(null, "Graph", OverlayGraph.entries, s.overlayGraph, { it.label }) { s.overlayGraph = it }
                        ChoiceRow(null, "Position", OverlayCorner.entries, s.overlayCorner, { it.glyph }, s.overlayCorner.label) { s.overlayCorner = it }
                        Column(Modifier.padding(start = 54.dp, end = 16.dp, top = 8.dp, bottom = 8.dp)) {
                            Text("Background", style = MaterialTheme.typography.bodyLarge)
                            Slider(s.overlayOpacity, { s.overlayOpacity = it }, valueRange = 0f..0.9f)
                        }
                        SwitchRow(null, "Show in device view", "Also show it on the drawn device, not only in full screen", s.overlayInDeviceView) { s.overlayInDeviceView = it }
                    }
                }
                SettingsGroup("Diagnostics") {
                    SwitchRow(LtIcons.Timeline, "Inspect button", "A button that pauses iOS and opens the frame inspector, to pick apart a hitch you just saw", s.hitchButton, "hitch_toggle") { s.hitchButton = it }
                    SwitchRow(LtIcons.Chip, "Detailed frame timing", "Records each frame's guest CPU, GL and lock time so the overlay and inspector can explain slow frames. Costs about 6% speed while on.", s.detailedTiming, "detailed_timing") { s.detailedTiming = it; host.applySettings() }
                    if (!s.detailedTiming) ActionRow(LtIcons.Save, if (host.profileRecording) "Stop profile recording" else "Record a profile",
                        if (host.profileRecording) host.profileNotice.ifEmpty { "Recording…" } else "Capture engine timing for the app on screen, for tools/perf/profile.py",
                        enabled = !host.profileBusy && (host.running || host.profileRecording), tag = "profile_capture") { host.toggleProfile() }
                    if (host.selected != null && !host.active) ActionRow(LtIcons.History, "Recover retained state",
                        "Start ${host.selected?.name} from its saved data after an unclean shutdown, keeping diagnostics", enabled = !host.busy, tag = "recover_guest") { host.recover(); onBack() }
                }
                SettingsGroup("Device") {
                    ActionRow(LtIcons.Puzzle, "Update guest tools", host.selected?.let { "Install a newer .itpack into ${it.name}; it applies at the next start" } ?: "Select a device first",
                        enabled = host.selected != null && !host.active && !host.busy) { host.pickGuestTools() }
                    ActionRow(LtIcons.Download, "Prepare from an IPSW", "Choose a device and iOS version; LightTouch downloads Apple's firmware and builds it here",
                        enabled = !host.active && !host.busy) { host.openPreparation() }
                }
                SettingsGroup("About") {
                    ActionRow(LtIcons.Info, "LightTouch ${BuildConfig.VERSION_NAME}", "Classic iOS for Android") {}
                    ActionRow(LtIcons.Code, "Source code", "github.com/blake-lucas/LightTouch") {
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://github.com/blake-lucas/LightTouch")))
                    }
                    ActionRow(LtIcons.File, "Open source licenses", null, tag = "open_licenses") { host.openLicenses() }
                }
                Spacer(Modifier.height(24.dp))
            }
        }
    }
}

@Composable
internal fun SettingsGroup(title: String, content: @Composable ColumnScope.() -> Unit) {
    Text(title, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 20.dp, bottom = 8.dp))
    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(vertical = 6.dp), content = content)
}

@Composable
private fun RowIcon(icon: ImageVector?, alpha: Float = 1f) {
    if (icon != null) { Icon(icon, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = alpha)); Spacer(Modifier.width(16.dp)) }
    else Spacer(Modifier.width(38.dp))
}

@Composable
internal fun SwitchRow(icon: ImageVector?, title: String, subtitle: String?, checked: Boolean, tag: String? = null,
                      info: (() -> Unit)? = null, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth().clickable { onChange(!checked) }.padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        RowIcon(icon)
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            subtitle?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
        if (info != null) IconButton(onClick = info) { Icon(LtIcons.Info, "About this setting", tint = MaterialTheme.colorScheme.onSurfaceVariant) }
        else Spacer(Modifier.width(12.dp))
        Switch(checked, onChange, modifier = if (tag != null) Modifier.testTag(tag) else Modifier)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun <T> ChoiceRow(icon: ImageVector?, title: String, options: List<T>, selected: T, label: (T) -> String,
                          subtitle: String? = null, tag: String? = null, onSelect: (T) -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            RowIcon(icon)
            Column {
                Text(title, style = MaterialTheme.typography.bodyLarge)
                subtitle?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            }
        }
        Spacer(Modifier.height(8.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(start = 38.dp).then(if (tag != null) Modifier.testTag(tag) else Modifier)) {
            options.forEachIndexed { i, option ->
                SegmentedButton(selected = option == selected, onClick = { onSelect(option) }, shape = SegmentedButtonDefaults.itemShape(i, options.size),
                    icon = {}, label = { Text(label(option), maxLines = 1, style = MaterialTheme.typography.labelMedium) })
            }
        }
    }
}

@Composable
internal fun ActionRow(icon: ImageVector?, title: String, subtitle: String?, enabled: Boolean = true, tag: String? = null, onClick: () -> Unit) {
    Row(Modifier.fillMaxWidth().clickable(enabled = enabled, onClick = onClick).then(if (tag != null) Modifier.testTag(tag) else Modifier)
        .padding(horizontal = 16.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
        val alpha = if (enabled) 1f else 0.4f
        RowIcon(icon, alpha)
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurface.copy(alpha = alpha))
            subtitle?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = alpha)) }
        }
        Icon(Icons.Filled.KeyboardArrowRight, null, tint = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = alpha * 0.6f))
    }
}

/** The overlay as it will look, over a stand-in screen, with live numbers when iOS is running. */
@Composable
private fun OverlayPreview(host: GuestHost) {
    val s = host.settings
    val sample = if (host.running && host.perf.fps > 0) host.perf else remember {
        PerfSnapshot(fps = 59f, frameMs = 16.9f, lowFps = 48f, maxFrameMs = 24.1f, presentMs = 0.4f, latencyMs = 3.2f, vcpuPercent = 86f,
            vcpuCore = 7, vcpuMHz = 2840, workerPercent = 131f, pssMiB = 412f, glMsPerFrame = 3.1f, bqlMsPerFrame = 0.3f, runPercent = 82f,
            intervals = FloatArray(160) { i -> 16.7f + (if (i % 37 == 0) 18f else 0f) + (i % 5) * 0.4f }, detailedAvailable = s.detailedTiming)
    }
    Box(Modifier.padding(start = 54.dp, end = 16.dp, top = 4.dp, bottom = 8.dp).fillMaxWidth().height(when (s.overlay) { OverlayLevel.FULL -> 196.dp; OverlayLevel.COMPACT -> 72.dp; else -> 48.dp })
        .clip(RoundedCornerShape(14.dp)).background(Color(0xFF1B3A4B)).testTag("overlay_preview")) {
        PerformanceOverlay(sample, s.overlay, s.overlayGraph, s.overlayOpacity, s.detailedTiming,
            Modifier.align(Alignment.of(s.overlayCorner)).padding(8.dp))
    }
}
