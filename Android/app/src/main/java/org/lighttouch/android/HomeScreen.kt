package org.lighttouch.android

import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.fadeOut
import androidx.compose.animation.fadeIn
import androidx.compose.animation.expandVertically
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.AnimatedContent
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The default view: the selected device drawn life-size-ish with its guest screen, a top bar for
 * switching devices and reaching the Store and Settings, and a dock with the session controls.
 */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun HomeScreen(host: GuestHost, guestSurface: @Composable (Modifier) -> Unit) {
    var confirmStop by remember { mutableStateOf(false) }
    val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
    Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background).semantics { testTagsAsResourceId = true }) {
        // A soft spotlight behind the device.
        Box(Modifier.fillMaxSize().background(Brush.radialGradient(
            listOf(MaterialTheme.colorScheme.primary.copy(alpha = if (dark) 0.07f else 0.10f), Color.Transparent))))
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            TopBar(host)
            Box(Modifier.weight(1f).fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp), contentAlignment = Alignment.Center) {
                DeviceFrame(DeviceSkin.forBoard(host.selected?.board), enabled = host.running, button = host::button,
                    screen = { modifier -> GuestScreen(host, guestSurface, modifier, overlayScale = 0.85f, inDeviceView = true) },
                    modifier = Modifier.fillMaxSize(), haptics = host.settings.haptics)
            }
            // Stopped, the screen itself offers Start or Recover; the dock is for a running session.
            AnimatedVisibility(host.active, enter = expandVertically() + fadeIn(), exit = shrinkVertically() + fadeOut()) { Dock(host) { confirmStop = true } }
            if (!host.active) Spacer(Modifier.height(16.dp))
        }
        // Floats over the device under the top bar, so a message never resizes the guest screen.
        ErrorBanner(host.error, host::dismissError, Modifier.align(Alignment.TopCenter).windowInsetsPadding(WindowInsets.safeDrawing)
            .padding(start = 16.dp, end = 16.dp, top = 64.dp))
        // For automation (tools/perf): the raw session state as text.
        Text(host.status, Modifier.align(Alignment.TopStart).size(1.dp).testTag("session_status"), color = Color.Transparent, fontSize = 1.sp)
    }
    if (confirmStop) StopDialog(host) { confirmStop = false }
}

@Composable
private fun TopBar(host: GuestHost) {
    Row(Modifier.fillMaxWidth().height(64.dp).padding(start = 12.dp, end = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        val device = host.selected
        Box(Modifier.weight(1f)) { Row(Modifier.clip(RoundedCornerShape(12.dp)).clickable { host.openDevices() }
            .padding(horizontal = 8.dp, vertical = 6.dp).testTag("device_selector"), verticalAlignment = Alignment.CenterVertically) {
            StatusDot(host.status)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f, fill = false)) {
                Text(device?.name ?: "No device", style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(listOfNotNull(device?.subtitle, statusLabel(host.status)).joinToString(" · "), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Icon(Icons.Filled.KeyboardArrowDown, "Devices", Modifier.padding(start = 4.dp).size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        } }
        IconButton(onClick = { host.navigate(Screen.STORE) }, modifier = Modifier.testTag("open_store")) { Icon(LtIcons.Bag, "Legacy Store") }
        IconButton(onClick = { host.navigate(Screen.SETTINGS) }, modifier = Modifier.testTag("open_settings")) { Icon(Icons.Filled.Settings, "Settings") }
    }
}

@Composable
internal fun StatusDot(status: String) {
    val color = when (status) {
        "running" -> LtColors.Accent; "starting", "stopping" -> LtColors.Warning; "paused" -> Color(0xFF9AA2A6)
        "failed" -> LtColors.Danger; else -> MaterialTheme.colorScheme.outline
    }
    Box(Modifier.size(10.dp).background(color, CircleShape))
}

/**
 * The guest surface plus whatever belongs on top of it: the performance overlay and, when the
 * guest is not simply running, a state card (start, starting, paused, asleep, failed).
 */
@Composable
internal fun GuestScreen(host: GuestHost, guestSurface: @Composable (Modifier) -> Unit, modifier: Modifier,
                         overlayScale: Float, inDeviceView: Boolean) {
    Box(modifier.background(Color.Black).testTag("guest_screen")) {
        guestSurface(Modifier.fillMaxSize())
        val s = host.settings
        if (host.running && (!inDeviceView || s.overlayInDeviceView))
            PerformanceOverlay(host.perf, s.overlay, s.overlayGraph, s.overlayOpacity, s.detailedTiming,
                Modifier.align(Alignment.of(s.overlayCorner)).padding(6.dp), scale = overlayScale)
        val state = when {
            host.status == "running" && host.displayAsleep -> "asleep"
            host.status == "running" -> null
            else -> host.status
        }
        // An asleep display is black, as on the device and on every board: the engine sends no frames while it is
        // off, and the iPod touch 3G's and iPhone 3GS's models don't dim the last one with the backlight.
        AnimatedContent(state, Modifier.matchParentSize(), label = "guest-state") { shown ->
            if (shown != null) Box(Modifier.fillMaxSize().background(if (shown == "asleep") Color.Black else Color.Black.copy(alpha = 0.7f)),
                contentAlignment = Alignment.Center) {
                when (shown) {
                    "starting" -> Busy("Starting iOS…")
                    "stopping" -> Busy("Shutting down…")
                    "paused" -> ScreenAction(LtIcons.Play, "Paused · tap to resume", { host.setPaused(false) }, tag = "resume_overlay")
                    "asleep" -> ScreenAction(LtIcons.Power, "Screen off · tap to wake", { host.pulse(GuestButton.HOME) }, tag = "guest_screen_asleep")
                    "failed" -> Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.padding(16.dp)) {
                        Icon(Icons.Filled.Warning, null, tint = LtColors.Warning, modifier = Modifier.size(32.dp))
                        Spacer(Modifier.height(8.dp))
                        Text("iOS stopped unexpectedly", color = Color.White, style = MaterialTheme.typography.titleSmall, textAlign = TextAlign.Center)
                        Spacer(Modifier.height(12.dp))
                        if (host.sessionOwned) TextButton(onClick = host::stop) { Text("Close session") }
                        else ScreenAction(LtIcons.History, "Recover", host::recover, tag = "recover_guest")
                    }
                    else -> if (host.selected == null) ScreenAction(LtIcons.Phone, "Add a device", host::addDevice, tag = "add_device")
                        else if (host.selected?.dirty == true) ScreenAction(LtIcons.History, "Recover and start", { if (!host.busy) host.recover() }, tag = "recover_guest")
                        else ScreenAction(LtIcons.Play, "Start", { if (!host.busy) host.start() }, tag = "start_guest")
                }
            }
        }
    }
}

@Composable
private fun Busy(label: String) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        CircularProgressIndicator(color = Color.White, strokeWidth = 3.dp, modifier = Modifier.size(36.dp))
        Spacer(Modifier.height(14.dp))
        Text(label, color = Color.White.copy(alpha = 0.85f), style = MaterialTheme.typography.labelLarge)
    }
}

@Composable
private fun Dock(host: GuestHost, onStop: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surfaceContainer, shape = RoundedCornerShape(24.dp), tonalElevation = 2.dp,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp).testTag("dock")) {
        Row(Modifier.padding(horizontal = 8.dp, vertical = 4.dp), horizontalArrangement = Arrangement.SpaceEvenly) {
            val canControl = host.status in listOf("running", "paused")
            DockButton(if (host.paused) LtIcons.Play else LtIcons.Pause, if (host.paused) "Resume" else "Pause",
                { host.setPaused(!host.paused) }, Modifier.weight(1f), enabled = canControl, tag = "pause_guest")
            DockButton(LtIcons.Fullscreen, "Full screen", { host.navigate(Screen.FULLSCREEN) }, Modifier.weight(1f), tag = "fullscreen_button")
            DockButton(LtIcons.Camera, "Screenshot", host::screenshot, Modifier.weight(1f), enabled = canControl, tag = "screenshot_button")
            if (host.settings.hitchButton) DockButton(LtIcons.Timeline, "Inspect", host::openHitch, Modifier.weight(1f), enabled = canControl, tag = "hitch_button")
            DockButton(LtIcons.Stop, "Stop", onStop, Modifier.weight(1f),
                enabled = host.sessionOwned && host.status != "stopping" && !host.storeInstalling, tint = LtColors.Danger, tag = "stop_guest")
        }
    }
}

/** Shutting down is slow and closes every app, so it is confirmed; Save & Stop keeps the session. */
@Composable
internal fun StopDialog(host: GuestHost, onDismiss: () -> Unit) {
    AlertDialog(onDismissRequest = onDismiss, modifier = DialogTags,
        icon = { Icon(LtIcons.Power, null) },
        title = { Text("Stop ${host.selected?.name ?: "device"}?", style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center) },
        text = {
            Column {
                Text(if (host.canSaveState) "Save & Stop keeps everything as it is, open apps included, and picks up right here next time."
                    else "iOS shuts down like holding the power button: apps close and the device turns off. Your apps and data are kept.",
                    style = MaterialTheme.typography.bodyMedium)
            }
        },
        confirmButton = {
            if (host.canSaveState) Button(onClick = { onDismiss(); host.saveAndStop() }, modifier = Modifier.testTag("save_and_stop")) { Text("Save & Stop") }
            else Button(onClick = { onDismiss(); host.stop() }, modifier = Modifier.testTag("confirm_stop"),
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error)) { Text("Shut down") }
        },
        dismissButton = {
            Row {
                if (host.canSaveState) TextButton(onClick = { onDismiss(); host.stop() }, modifier = Modifier.testTag("confirm_stop")) { Text("Shut down") }
                TextButton(onClick = onDismiss) { Text("Cancel") }
            }
        })
}

internal fun Color.luminance() = 0.2126f * red + 0.7152f * green + 0.0722f * blue
