package org.lighttouch.android

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.min
import kotlinx.coroutines.delay

/**
 * Full screen: the guest as large as the panel allows at its own aspect, with no device drawn.
 * On a tall phone the space under the guest holds the hardware buttons, where thumbs already are;
 * when there is no room for them they float over the guest and fade until the handle is tapped.
 */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun FullscreenScreen(host: GuestHost, guestSurface: @Composable (Modifier) -> Unit) = DarkTheme {
    BackHandler { host.navigate(Screen.HOME) }
    var confirmStop by remember { mutableStateOf(false) }
    val skin = DeviceSkin.forBoard(host.selected?.board)
    val aspect = skin.screenWidth / skin.screenHeight
    BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black).semantics { testTagsAsResourceId = true }) {
        // The guest is centred on the panel itself, with equal black borders above and below, so it
        // sits in the middle of the phone however it is held. The buttons dock in the lower border
        // when it can hold them; the guest shrinks a little to make room before they float instead.
        val bar = 88.dp
        val fitHeight = maxWidth / aspect
        val docked = (maxHeight - fitHeight) / 2 >= bar * 0.8f
        val screenHeight = if (docked) min(fitHeight, maxHeight - bar * 2) else min(fitHeight, maxHeight)
        val screenWidth = screenHeight * aspect
        if (docked) {
            GuestScreen(host, guestSurface, Modifier.size(screenWidth, screenHeight).align(Alignment.Center), overlayScale = 1f, inDeviceView = false)
            Box(Modifier.align(Alignment.BottomCenter).fillMaxWidth().height((maxHeight - screenHeight) / 2), contentAlignment = Alignment.Center) {
                ButtonBar(host, Modifier.fillMaxWidth().height(64.dp)) { confirmStop = true }
            }
        } else {
            var visible by remember { mutableStateOf(true) }
            var touched by remember { mutableIntStateOf(0) }
            LaunchedEffect(visible, touched) { if (visible) { delay(4_000); visible = false } }
            GuestScreen(host, guestSurface, Modifier.size(screenWidth, screenHeight).align(Alignment.Center), overlayScale = 1f, inDeviceView = false)
            AnimatedVisibility(visible, Modifier.align(Alignment.BottomCenter), enter = fadeIn(), exit = fadeOut()) {
                ButtonBar(host, Modifier.padding(16.dp).background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(28.dp))
                    .height(88.dp).widthIn(max = 420.dp).fillMaxWidth(), onInteract = { touched++ }) { confirmStop = true }
            }
            if (!visible) Box(Modifier.align(Alignment.BottomCenter).padding(bottom = 10.dp).size(72.dp, 28.dp).clip(RoundedCornerShape(14.dp))
                .clickable { visible = true }.testTag("show_controls").semantics { contentDescription = "Show controls" }, contentAlignment = Alignment.Center) {
                Box(Modifier.size(40.dp, 5.dp).background(Color.White.copy(alpha = 0.45f), RoundedCornerShape(3.dp)))
            }
        }
        Text(host.status, Modifier.size(1.dp).testTag("session_status"), color = Color.Transparent)
        ErrorBanner(host.error, host::dismissError, Modifier.align(Alignment.TopCenter).statusBarsPadding().padding(12.dp))
    }
    if (confirmStop) StopDialog(host) { confirmStop = false }
}

@Composable
private fun ButtonBar(host: GuestHost, modifier: Modifier, onInteract: () -> Unit = {}, onStop: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    val enabled = host.running
    Row(modifier.padding(horizontal = 20.dp), verticalAlignment = Alignment.CenterVertically) {
        // Volume on the left, like the device's left edge.
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
            HardwareKey(LtIcons.VolumeDown, "Volume down", GuestButton.VOLUME_DOWN, host, enabled, onInteract, tag = "fs_volume_down")
            Spacer(Modifier.width(8.dp))
            HardwareKey(LtIcons.VolumeUp, "Volume up", GuestButton.VOLUME_UP, host, enabled, onInteract, tag = "fs_volume_up")
        }
        HomeKey(host, enabled, onInteract)
        Row(Modifier.weight(1f), horizontalArrangement = Arrangement.End, verticalAlignment = Alignment.CenterVertically) {
            if (host.settings.hitchButton) {
                SmallKey(LtIcons.Timeline, "Inspect frames", { onInteract(); host.openHitch() }, host.status in listOf("running", "paused"), tag = "hitch_button")
                Spacer(Modifier.width(8.dp))
            }
            HardwareKey(LtIcons.Power, "Sleep/Wake", GuestButton.POWER, host, enabled, onInteract, tag = "power_button")
            Spacer(Modifier.width(8.dp))
            Box {
                SmallKey(Icons.Filled.MoreVert, "More", { onInteract(); menu = true }, true, tag = "fullscreen_menu")
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem(text = { Text("Exit full screen") }, leadingIcon = { Icon(LtIcons.FullscreenExit, null) },
                        onClick = { menu = false; host.navigate(Screen.HOME) }, modifier = Modifier.testTag("exit_fullscreen"))
                    DropdownMenuItem(text = { Text(if (host.paused) "Resume" else "Pause") }, leadingIcon = { Icon(if (host.paused) LtIcons.Play else LtIcons.Pause, null) },
                        onClick = { menu = false; host.setPaused(!host.paused) }, enabled = host.status in listOf("running", "paused"))
                    DropdownMenuItem(text = { Text("Screenshot") }, leadingIcon = { Icon(LtIcons.Camera, null) },
                        onClick = { menu = false; host.screenshot() }, enabled = host.status in listOf("running", "paused"))
                    DropdownMenuItem(text = { Text("Inspect frames") }, leadingIcon = { Icon(LtIcons.Timeline, null) },
                        onClick = { menu = false; host.openHitch() }, enabled = host.status in listOf("running", "paused"))
                    DropdownMenuItem(text = { Text("Performance overlay: ${host.settings.overlay.label}") }, leadingIcon = { Icon(LtIcons.Speed, null) },
                        onClick = { host.settings.overlay = OverlayLevel.entries[(host.settings.overlay.ordinal + 1) % OverlayLevel.entries.size] })
                    DropdownMenuItem(text = { Text("Settings") }, leadingIcon = { Icon(Icons.Filled.Settings, null) },
                        onClick = { menu = false; host.navigate(Screen.SETTINGS) })
                    HorizontalDivider()
                    DropdownMenuItem(text = { Text(if (host.status == "stopping") "Force stop" else "Stop", color = LtColors.Danger) }, leadingIcon = { Icon(LtIcons.Stop, null, tint = LtColors.Danger) },
                        onClick = { menu = false; onStop() }, enabled = host.sessionOwned && !host.storeInstalling)
                }
            }
        }
    }
}

/** A guest hardware button: pressed while held, so press-and-hold gestures (power off) work. */
@Composable
private fun HardwareKey(icon: ImageVector, label: String, id: Int, host: GuestHost, enabled: Boolean, onInteract: () -> Unit, tag: String) {
    val haptic = LocalHapticFeedback.current
    Box(Modifier.size(48.dp).clip(CircleShape).background(Color.White.copy(alpha = 0.08f))
        .semantics { contentDescription = label; role = Role.Button }.testTag(tag)
        .held(enabled, id, { button, down -> if (down) onInteract(); host.button(button, down) }) { if (host.settings.haptics) haptic.performHapticFeedback(HapticFeedbackType.LongPress) },
        contentAlignment = Alignment.Center) {
        Icon(icon, null, Modifier.size(22.dp), tint = Color.White.copy(alpha = if (enabled) 0.85f else 0.3f))
    }
}

@Composable
private fun SmallKey(icon: ImageVector, label: String, onClick: () -> Unit, enabled: Boolean, tag: String) {
    Box(Modifier.size(44.dp).clip(CircleShape).clickable(enabled = enabled, onClick = onClick)
        .semantics { contentDescription = label }.testTag(tag), contentAlignment = Alignment.Center) {
        Icon(icon, null, Modifier.size(22.dp), tint = Color.White.copy(alpha = if (enabled) 0.75f else 0.3f))
    }
}

/** The Home button, drawn like the device's: a ring with a rounded square. */
@Composable
private fun HomeKey(host: GuestHost, enabled: Boolean, onInteract: () -> Unit, size: Dp = 64.dp) {
    val haptic = LocalHapticFeedback.current
    Box(Modifier.size(size).clip(CircleShape).background(Color.White.copy(alpha = 0.06f)).border(1.5.dp, Color.White.copy(alpha = if (enabled) 0.35f else 0.15f), CircleShape)
        .semantics { contentDescription = "Home"; role = Role.Button }.testTag("home_button")
        .held(enabled, GuestButton.HOME, { button, down -> if (down) onInteract(); host.button(button, down) }) { if (host.settings.haptics) haptic.performHapticFeedback(HapticFeedbackType.LongPress) },
        contentAlignment = Alignment.Center) {
        Box(Modifier.size(size * 0.3f).border(1.5.dp, Color.White.copy(alpha = if (enabled) 0.6f else 0.2f), RoundedCornerShape(size * 0.07f)))
    }
}
