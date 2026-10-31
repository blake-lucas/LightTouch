package org.lighttouch.android

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** Hardware button ids shared with the runtime (QEMU_IOS_BUTTON_*). */
internal object GuestButton { const val HOME = 0; const val POWER = 1; const val VOLUME_UP = 2; const val VOLUME_DOWN = 3 }

/**
 * Physical outline of the emulated device, in millimetres. The screen rectangle is placed so
 * the guest surface gets exactly the panel's aspect, so the presenter never letterboxes.
 */
internal data class DeviceSkin(
    val width: Float, val height: Float, val corner: Float,
    val screenLeft: Float, val screenTop: Float, val screenWidth: Float, val screenHeight: Float,
    val homeCenterY: Float, val homeDiameter: Float, val face: Color, val rim: List<Color>,
    /** The metal edge seen around the glass from the front. */
    val rimWidth: Float = 0.7f,
    /** Sleep/Wake on the top edge: its left end and length; null keeps the phones' proportions. */
    val power: Pair<Float, Float>? = null,
    /** Volume on the right edge (the iPad), and each button's top and length; null keeps the phones'. */
    val volumeRight: Boolean = false, val volumeUp: Pair<Float, Float>? = null, val volumeDown: Pair<Float, Float>? = null,
) {
    companion object {
        // iPod touch 2nd generation: 110 x 61.8 mm, 3.5" 320x480 panel, black glass, chrome back.
        val ipodTouch2 = DeviceSkin(61.8f, 110f, 8.5f, 5.95f, 17.6f, 49.9f, 74.8f, 101.4f, 10.5f,
            Color(0xFF0B0B0C), listOf(Color(0xFFE8E8EA), Color(0xFF8E8E93), Color(0xFFD1D1D6)))
        // iPod touch 4th generation: 111 x 58.9 mm, 3.5" panel, flatter rim.
        val ipodTouch4 = DeviceSkin(58.9f, 111f, 7.5f, 4.5f, 18.1f, 49.9f, 74.8f, 102.3f, 9.6f,
            Color(0xFF0B0B0C), listOf(Color(0xFFD9D9DC), Color(0xFF9A9AA0), Color(0xFFC7C7CC)))
        val iphone = DeviceSkin(62.1f, 115.5f, 9f, 6.1f, 20.3f, 49.9f, 74.8f, 105.6f, 10.5f,
            Color(0xFF0B0B0C), listOf(Color(0xFFC7C7CC), Color(0xFF6E6E73), Color(0xFFAEAEB2)))
        // iPad (first generation): 242.8 x 189.7 mm, 9.7" 4:3 panel (197.1 x 147.8 mm) centred in the glass, a wide
        // aluminium rim, Sleep/Wake at the top right and the volume rocker on the right edge below the side switch.
        val ipad = DeviceSkin(189.7f, 242.8f, 13f, 20.95f, 22.85f, 147.8f, 197.1f, 231.4f, 11.5f,
            Color(0xFF0B0B0C), listOf(Color(0xFFD1D1D6), Color(0xFF8E8E93), Color(0xFFC7C7CC)),
            rimWidth = 2.4f, power = 156f to 12f, volumeRight = true, volumeUp = 45f to 12f, volumeDown = 57.5f to 12f)
        fun forBoard(board: String?): DeviceSkin = when {
            board == null -> ipodTouch2
            board.startsWith("n81") -> ipodTouch4
            board.startsWith("k48") -> ipad
            board.startsWith("m68") || board.startsWith("n82") || board.startsWith("n88") || board.startsWith("n90") -> iphone
            else -> ipodTouch2
        }
    }
}

/**
 * The guest screen inside a drawn device, scaled to the largest size that fits, with its
 * Home, Sleep/Wake and volume buttons as real controls (press and hold is forwarded as
 * held, so the power-off slider and screenshots work as on hardware).
 */
@Composable
internal fun DeviceFrame(skin: DeviceSkin, enabled: Boolean, button: (Int, Boolean) -> Unit, screen: @Composable (Modifier) -> Unit,
                         modifier: Modifier = Modifier, haptics: Boolean = true) {
    BoxWithConstraints(modifier, contentAlignment = Alignment.Center) {
        // Leave room around the body for the protruding side buttons.
        val margin = 3f
        val scale = minOf(maxWidth.value / (skin.width + 2 * margin), maxHeight.value / (skin.height + 2 * margin))
        fun mm(value: Float): Dp = (value * scale).dp
        // Sleep/Wake on the top edge, right of centre; volume on the upper left side. Each is
        // (left, top, width, height) in mm of the outer box, drawn under the body's edge.
        val side = skin.height * 0.065f
        val power = skin.power ?: (skin.width * 0.62f to skin.width * 0.2f)
        val up = skin.volumeUp ?: (skin.height * 0.13f to side)
        val down = skin.volumeDown ?: (skin.height * 0.21f to side)
        val volumeX = if (skin.volumeRight) margin + skin.width - 0.7f else margin - 1.3f
        val buttons = listOf(
            Triple(GuestButton.POWER, "Sleep/Wake", floatArrayOf(margin + power.first, margin - 1.4f, power.second, 2f)),
            Triple(GuestButton.VOLUME_UP, "Volume up", floatArrayOf(volumeX, margin + up.first, 2f, up.second)),
            Triple(GuestButton.VOLUME_DOWN, "Volume down", floatArrayOf(volumeX, margin + down.first, 2f, down.second)))
        val feedback = LocalHapticFeedback.current
        val haptic = { if (haptics) feedback.performHapticFeedback(HapticFeedbackType.LongPress) }
        Box(Modifier.size(mm(skin.width + 2 * margin), mm(skin.height + 2 * margin))) {
            for ((_, _, r) in buttons)
                Box(Modifier.offset(mm(r[0]), mm(r[1])).size(mm(r[2]), mm(r[3])).background(Brush.linearGradient(skin.rim), RoundedCornerShape(mm(0.8f))))
            val body = RoundedCornerShape(mm(skin.corner))
            Box(Modifier.offset(mm(margin), mm(margin)).size(mm(skin.width), mm(skin.height))
                .shadow(mm(4f), body, ambientColor = Color.Black, spotColor = Color.Black)
                .background(Brush.linearGradient(skin.rim), body).padding(mm(skin.rimWidth))
                .background(skin.face, RoundedCornerShape(mm(skin.corner - skin.rimWidth)))) {
                screen(Modifier.offset(mm(skin.screenLeft - skin.rimWidth), mm(skin.screenTop - skin.rimWidth)).size(mm(skin.screenWidth), mm(skin.screenHeight)))
                HomeButton(Modifier.offset(mm(skin.width / 2 - skin.homeDiameter / 2 - skin.rimWidth), mm(skin.homeCenterY - skin.homeDiameter / 2 - skin.rimWidth))
                    .size(mm(skin.homeDiameter)), enabled, button, haptic)
            }
            // A drawn side button is a few millimetres; its touch target is at least 48dp, centred on it.
            for ((id, label, r) in buttons) {
                val w = maxOf(mm(r[2]), 48.dp); val h = maxOf(mm(r[3]), 48.dp)
                Box(Modifier.offset(mm(r[0] + r[2] / 2) - w / 2, mm(r[1] + r[3] / 2) - h / 2).size(w, h)
                    .semantics { contentDescription = label; role = Role.Button }
                    .testTag("device_" + label.lowercase().replace(Regex("[^a-z]+"), "_"))
                    .held(enabled, id, button, haptic))
            }
        }
    }
}

internal fun Modifier.held(enabled: Boolean, id: Int, button: (Int, Boolean) -> Unit, haptic: () -> Unit) =
    pointerInput(enabled, id) {
        if (!enabled) return@pointerInput
        detectTapGestures(onPress = {
            haptic(); button(id, true)
            try { tryAwaitRelease() } finally { button(id, false) }
        })
    }

@Composable
private fun HomeButton(modifier: Modifier, enabled: Boolean, button: (Int, Boolean) -> Unit, haptic: () -> Unit) {
    val ring = with(LocalDensity.current) { 1.dp }
    Box(modifier.semantics { contentDescription = "Home"; role = Role.Button }.testTag("device_home")
        .background(Brush.radialGradient(listOf(Color(0xFF1C1C1E), Color(0xFF050505))), CircleShape)
        .border(ring, Color(0xFF3A3A3C), CircleShape)
        .held(enabled, GuestButton.HOME, button, haptic),
        contentAlignment = Alignment.Center) {
        BoxWithConstraints(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Box(Modifier.size(maxWidth * 0.34f).border(ring, Color(0xFF8E8E93), RoundedCornerShape(maxWidth * 0.08f)))
        }
    }
}
