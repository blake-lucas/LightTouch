package org.lighttouch.android

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.min
import kotlin.math.roundToInt

/** The screen's pixels standing upright, by board (the desktop's device_frame::screen_pixels). */
internal fun screenPixels(board: String?): IntSize = when {
    board == null -> IntSize(320, 480)
    board.startsWith("n81") || board.startsWith("n90") -> IntSize(640, 960)
    board.startsWith("n42") -> IntSize(640, 1136)
    board.startsWith("k48") -> IntSize(768, 1024)
    else -> IntSize(320, 480)
}

/** The largest whole multiple of [pixels] that fits a room of so many physical pixels; 0 when not even 1x does. */
internal fun largestMultiple(pixels: IntSize, roomWidth: Float, roomHeight: Float): Int =
    (12 downTo 1).firstOrNull { pixels.width * it <= roomWidth + 0.5f && pixels.height * it <= roomHeight + 0.5f } ?: 0

/**
 * Pixel perfect's multiple: the largest that fits (Auto, [fixed] 0), a [fixed] one capped at that, and never less than
 * 1x, so the screen is always a whole multiple, cropped when even 1x doesn't fit.
 */
internal fun wholeMultiple(pixels: IntSize, roomWidth: Float, roomHeight: Float, fixed: Int): Int {
    val largest = largestMultiple(pixels, roomWidth, roomHeight).coerceAtLeast(1)
    return if (fixed > 0) min(fixed, largest) else largest
}

/**
 * Crisp's prescale, as the presenter makes it: the largest whole multiple that fits (1 below 2x, as Smooth; the
 * desktop app, which prescales on its GPU's behalf, uses 2 there).
 */
internal fun crispMultiple(scale: Float): Int = floor(scale + 0.01f).toInt().coerceIn(1, 8)

/** Millimetres of room the device view leaves around the drawn device, for its side buttons. */
internal const val DEVICE_MARGIN = 3f

/** The device view's room in physical pixels as last laid out, so the Screen scaling preview can predict it. */
internal object DeviceViewRoom { var size by mutableStateOf(IntSize.Zero) }

/**
 * How the device view draws a device in a room of [width] x [height] dp: [scale] dp per millimetre, Pixel perfect's
 * [whole] multiple (0 for other methods), whether it's at its [real] size or [realTooLarge] for it, and whether the
 * device is [cropped] around a screen that fits when the whole device doesn't.
 */
internal data class DeviceViewFit(val scale: Float, val whole: Int, val real: Boolean, val cropped: Boolean, val realTooLarge: Boolean)

internal fun deviceViewFit(skin: DeviceSkin, pixels: IntSize, width: Float, height: Float, density: Float, method: ScreenScaling,
                           fixed: Int, realPxPerMm: Float?): DeviceViewFit {
    val m = DEVICE_MARGIN
    val fit = min(width / (skin.width + 2 * m), height / (skin.height + 2 * m))
    if (method == ScreenScaling.PIXEL_PERFECT) {
        // The largest multiple the whole device fits around, else the largest the screen alone fits (the device
        // cropped), else 1x; a fixed multiple is a cap on that.
        val device = largestMultiple(pixels, skin.screenWidth * fit * density, skin.screenHeight * fit * density)
        val screen = largestMultiple(pixels, width * density, height * density)
        val auto = if (device > 0) device else screen.coerceAtLeast(1)
        val whole = if (fixed > 0) min(fixed, auto) else auto
        return DeviceViewFit(whole * pixels.width / density / skin.screenWidth, whole, false, whole > device, false)
    }
    if (realPxPerMm != null) {
        val s = realPxPerMm / density
        if ((skin.width + 2 * m) * s <= width + 0.5f && (skin.height + 2 * m) * s <= height + 0.5f) return DeviceViewFit(s, 0, true, false, false)
        if (skin.screenWidth * s <= width + 0.5f && skin.screenHeight * s <= height + 0.5f) return DeviceViewFit(s, 0, true, true, false)
        return DeviceViewFit(fit, 0, false, false, true)
    }
    return DeviceViewFit(fit, 0, false, false, false)
}

/** Where full screen puts the guest: its size in physical pixels, and whether the button bar docks under it. */
internal data class FullscreenFit(val width: Float, val height: Float, val docked: Boolean)

/**
 * The guest's size in full screen (FullscreenScreen) in a window of [width] x [height] pixels: as wide as the panel at
 * the screen's aspect, with equal borders above and below; the [bar] docks in the lower one when it nearly fits, the
 * guest shrinking a little to make room. Pixel perfect takes a whole multiple of [pixels] in that room (at least 1x),
 * real size the measured density where the screen fits.
 */
internal fun fullscreenFit(skin: DeviceSkin, pixels: IntSize, width: Float, height: Float, bar: Float, method: ScreenScaling, fixed: Int,
                           realPxPerMm: Float? = null): FullscreenFit {
    val aspect = skin.screenWidth / skin.screenHeight
    val fitHeight = width / aspect
    val docked = (height - fitHeight) / 2 >= bar * 0.8f
    val room = if (docked) height - bar * 2 else height
    if (method == ScreenScaling.PIXEL_PERFECT) {
        val whole = wholeMultiple(pixels, width, room, fixed)
        return FullscreenFit((whole * pixels.width).toFloat(), (whole * pixels.height).toFloat(), docked)
    }
    if (realPxPerMm != null && skin.screenWidth * realPxPerMm <= width + 0.5f && skin.screenHeight * realPxPerMm <= room + 0.5f)
        return FullscreenFit(skin.screenWidth * realPxPerMm, skin.screenHeight * realPxPerMm, docked)
    val h = min(fitHeight, room)
    return FullscreenFit(h * aspect, h, docked)
}

/** The screens the preview can show. */
private enum class PreviewScreen(val label: String, val short: String, val pixels: IntSize, val skin: DeviceSkin, val ipad: Boolean = false) {
    CLASSIC("iPhone, iPod touch · 320 × 480", "320 × 480", IntSize(320, 480), DeviceSkin.ipodTouch2),
    RETINA("Retina · 640 × 960", "Retina", IntSize(640, 960), DeviceSkin.ipodTouch4),
    TALL("iPhone 5 · 640 × 1136", "iPhone 5", IntSize(640, 1136), DeviceSkin.iphone5),
    IPAD("iPad · 1024 × 768", "iPad", IntSize(768, 1024), DeviceSkin.ipad, ipad = true);

    companion object {
        fun forBoard(board: String?) = screenPixels(board).let { p -> entries.first { it.pixels == p } }
    }
}

/** The preview's pictures: samples (art/preview/scaling, drawn by tools/scaling-samples/make.py), or 1 cm squares. */
private enum class PreviewPicture(val label: String, val file: String?) {
    PIXEL_ART("Pixel art", "pixel"), GAME("3D game", "game"), HOME("Home screen", "home"), SQUARES("1 cm squares", null)
}

/** A compact choice for the preview's header: its value as a chip, the choices in a menu. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun <T> MenuChip(value: String, options: List<T>, label: (T) -> String, tag: String, onPick: (T) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        AssistChip(onClick = { open = true }, label = { Text(value, maxLines = 1) }, trailingIcon = { Icon(Icons.Filled.ArrowDropDown, null) },
            modifier = Modifier.testTag(tag))
        DropdownMenu(open, onDismissRequest = { open = false }) {
            for (option in options) DropdownMenuItem(text = { Text(label(option)) }, onClick = { onPick(option); open = false })
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class, androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun ScreenScalingScreen(host: GuestHost, onBack: () -> Unit) {
    var measuring by rememberSaveable { mutableStateOf(false) }
    if (measuring) { MeasureDisplayScreen(host) { measuring = false }; return }
    BackHandler(onBack = onBack)
    var previewing by rememberSaveable { mutableStateOf(false) }
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }, color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(start = 4.dp, end = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack, modifier = Modifier.testTag("scaling_back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                Text("Screen scaling", style = MaterialTheme.typography.titleLarge, maxLines = 1, overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f))
                FilledTonalButton(onClick = { previewing = !previewing }, modifier = Modifier.testTag("scaling_preview")) {
                    Text(if (previewing) "Hide preview" else "Preview")
                }
            }
            if (previewing) ScalingPreview(host, Modifier.padding(horizontal = 16.dp).fillMaxWidth().weight(1f))
            Column(Modifier.fillMaxWidth().then(if (previewing) Modifier.wrapContentHeight() else Modifier.weight(1f))
                .verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).testTag("scaling_methods")) {
                if (!previewing) {
                    SettingsGroup("Method") { Methods(host) }
                    SettingsGroup("Real size") { RealSizeRows(host) { measuring = true } }
                } else {
                    Spacer(Modifier.height(12.dp))
                    Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainer)
                        .padding(vertical = 4.dp)) { Methods(host, compact = true) }
                }
                Spacer(Modifier.height(16.dp))
            }
        }
    }
}

/** The methods as a list to pick from, with Pixel perfect's multiple under it when it's picked. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun Methods(host: GuestHost, compact: Boolean = false) {
    val s = host.settings
    if (compact) {
        // Under the preview: one row of methods, so the picture keeps the room to show its real multiple.
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp).testTag("scaling_methods_row")) {
            ScreenScaling.entries.forEachIndexed { i, method ->
                SegmentedButton(selected = s.scaling == method, onClick = { s.scaling = method; host.applySettings() },
                    shape = SegmentedButtonDefaults.itemShape(i, ScreenScaling.entries.size), icon = {},
                    modifier = Modifier.testTag("scaling_" + method.name.lowercase()),
                    label = { Text(method.label, maxLines = 1, softWrap = false, style = MaterialTheme.typography.labelSmall.copy(letterSpacing = 0.sp)) })
            }
        }
        // Real size beside the methods, so the preview can compare it; Pixel perfect sets its own size, so its
        // multiples take this row instead.
        if (s.scaling != ScreenScaling.PIXEL_PERFECT) Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically) {
            Text("Real size", style = MaterialTheme.typography.labelMedium, modifier = Modifier.padding(start = 4.dp, end = 12.dp))
            SingleChoiceSegmentedButtonRow(Modifier.weight(1f).testTag("real_size_row")) {
                RealSize.entries.forEachIndexed { i, option ->
                    SegmentedButton(selected = s.realSize == option, onClick = { s.realSize = option },
                        shape = SegmentedButtonDefaults.itemShape(i, RealSize.entries.size), icon = {},
                        label = { Text(option.label, maxLines = 1, style = MaterialTheme.typography.labelSmall) })
                }
            }
        }
    }
    for (method in ScreenScaling.entries) {
        if (compact && method != ScreenScaling.PIXEL_PERFECT) continue
        val selected = s.scaling == method
        if (!compact) Row(Modifier.fillMaxWidth().selectable(selected, role = Role.RadioButton) { s.scaling = method; host.applySettings() }
            .padding(horizontal = 12.dp, vertical = 8.dp).testTag("scaling_" + method.name.lowercase()),
            verticalAlignment = Alignment.CenterVertically) {
            RadioButton(selected, onClick = null)
            Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(method.label, style = MaterialTheme.typography.bodyLarge)
                Text(method.about, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (method == ScreenScaling.PIXEL_PERFECT && selected) {
            val sizes = listOf(0 to "Auto", 1 to "1×", 2 to "2×", 3 to "3×", 4 to "4×")
            SingleChoiceSegmentedButtonRow(Modifier.padding(start = if (compact) 8.dp else 52.dp, end = if (compact) 8.dp else 12.dp, bottom = if (compact) 4.dp else 8.dp)
                .then(if (compact) Modifier.fillMaxWidth() else Modifier).testTag("scaling_size")) {
                sizes.forEachIndexed { i, (k, label) ->
                    SegmentedButton(selected = s.integerScale == k, onClick = { s.integerScale = k }, shape = SegmentedButtonDefaults.itemShape(i, sizes.size),
                        icon = {}, label = { Text(label, maxLines = 1, style = MaterialTheme.typography.labelMedium) })
                }
            }
        }
    }
}

/** Real size: which devices show at the real thing's size, the note when one doesn't fit, and the display's measure. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RealSizeRows(host: GuestHost, onMeasure: () -> Unit) {
    val s = host.settings
    val integer = s.scaling == ScreenScaling.PIXEL_PERFECT
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(LtIcons.Tablet, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.width(16.dp))
            Column {
                Text("Show at real size", style = MaterialTheme.typography.bodyLarge)
                Text(if (integer) "Pixel perfect sets its own size" else "The real device's size, for toys and pieces used on the screen",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Spacer(Modifier.height(8.dp))
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(start = 38.dp).testTag("real_size")) {
            RealSize.entries.forEachIndexed { i, option ->
                SegmentedButton(selected = s.realSize == option, onClick = { s.realSize = option }, enabled = !integer,
                    shape = SegmentedButtonDefaults.itemShape(i, RealSize.entries.size), icon = {},
                    label = { Text(option.label, maxLines = 1, style = MaterialTheme.typography.labelMedium) })
            }
        }
        // The selected device doesn't fit this phone at its real size: the device view fits it instead.
        val device = host.selected
        val room = DeviceViewRoom.size
        val px = s.realPxPerMm(device?.board)
        if (px != null && room.width > 0) {
            val skin = DeviceSkin.forBoard(device?.board)
            if (skin.screenWidth * px > room.width + 0.5f || skin.screenHeight * px > room.height + 0.5f)
                Text("${device?.model ?: "This device"} is larger than this screen at its real size, so it's fitted instead.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 38.dp, top = 8.dp))
        }
    }
    val measured = s.displayPxPerMm > 0f
    ActionRow(LtIcons.Phone, "Measure this display",
        "%.1f pixels per inch, %s".format(s.pxPerMm * 25.4f, if (measured) "measured" else "as the system reports"),
        tag = "measure_display", onClick = onMeasure)
}

/**
 * The preview predicts the device view: the size it shows the selected screen at (Pixel perfect's multiple, real
 * size, or fitted), drawn by the same method, on whole physical pixels. When that's larger than the box, Pixel
 * perfect drops to the largest multiple that fits and other methods are fitted, and the caption above says so.
 * 1 cm squares are always at this phone's measured density, to check the measure with a ruler.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun ScalingPreview(host: GuestHost, modifier: Modifier) {
    val context = LocalContext.current
    val s = host.settings
    var screen by rememberSaveable { mutableStateOf(PreviewScreen.forBoard(host.selected?.board)) }
    var picture by rememberSaveable { mutableStateOf(PreviewPicture.PIXEL_ART) }
    // Turned by its rotate button: 0 portrait, 1 Home on the right, 2 Home on the left (the device page's order).
    var turned by rememberSaveable { mutableStateOf(0) }
    var full by remember { mutableStateOf(false) }
    val landscape = turned != 0
    val degrees = when (turned) { 1 -> -90f; 2 -> 90f; else -> 0f }
    val squares = picture == PreviewPicture.SQUARES
    val method = s.scaling
    val source = remember(screen, picture) {
        val file = picture.file ?: PreviewPicture.PIXEL_ART.file
        try {
            context.assets.open("scaling/$file-${screen.pixels.width}x${screen.pixels.height}.png").use { BitmapFactory.decodeStream(it) }
        } catch (_: Exception) { Bitmap.createBitmap(screen.pixels.width, screen.pixels.height, Bitmap.Config.ARGB_8888) }
    }
    // The device view's room: as last laid out, else the window less the device page's bars.
    val density = LocalDensity.current.density
    val window = LocalConfiguration.current
    val room = DeviceViewRoom.size.takeIf { it.width > 0 }
        ?: IntSize(((window.screenWidthDp - 40) * density).toInt(), ((window.screenHeightDp - 260) * density).toInt())
    val realPx = when {
        method == ScreenScaling.PIXEL_PERFECT || s.realSize == RealSize.OFF -> null
        s.realSize == RealSize.IPADS && !screen.ipad -> null
        else -> s.pxPerMm
    }
    // Turned, the device view's room turns with it: its width is the screen's height.
    val (roomW, roomH) = if (landscape) room.height to room.width else room.width to room.height
    val view = deviceViewFit(screen.skin, screen.pixels, roomW / density, roomH / density, density, method, s.integerScale, realPx)
    val shownPixels = if (landscape) IntSize(screen.pixels.height, screen.pixels.width) else screen.pixels
    // How many physical pixels the device view gives each of the screen's pixels.
    val viewScale = if (view.whole > 0) view.whole.toFloat() else view.scale * density * screen.skin.screenWidth / screen.pixels.width
    Column(modifier) {
        // One row of choices (which screen, which picture) and one line saying what's shown: the picture gets the rest.
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            MenuChip(screen.short, PreviewScreen.entries, { it.label }, "preview_screen") { screen = it }
            MenuChip(picture.label, PreviewPicture.entries, { it.label }, "preview_picture_choice") { picture = it }
        }
        BoxWithConstraints(Modifier.fillMaxWidth().weight(1f)) {
            val box = with(LocalDensity.current) { IntSize(maxWidth.roundToPx(), maxHeight.roundToPx() - 28.dp.roundToPx()) }
            val (scale, caption) = if (squares) {
                s.pxPerMm * screen.skin.screenWidth / screen.pixels.width to
                    "1 cm squares · %.0f pixels per inch, %s".format(s.pxPerMm * 25.4f, if (s.displayPxPerMm > 0f) "measured" else "as the system reports")
            } else previewScale(method, shownPixels, viewScale, view, s.integerScale, box)
            Column {
                Text(caption, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1,
                    overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(start = 4.dp, bottom = 6.dp).height(22.dp).testTag("preview_caption"))
                Box(Modifier.fillMaxSize().clip(RoundedCornerShape(16.dp)).background(Color(0xFF0E0F12)).testTag("preview_picture")) {
                    if (squares) Squares(screen, s.pxPerMm, landscape, Modifier.fillMaxSize())
                    else ScaledPicture(source, method, scale, degrees, Modifier.fillMaxSize())
                    // Full screen (the sample as full screen draws it) and Rotate, over the picture's top-right corner.
                    Row(Modifier.align(Alignment.TopEnd).padding(8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (!squares) FilledTonalIconButton(onClick = { full = true }, modifier = Modifier.size(40.dp).testTag("preview_fullscreen")) {
                            Icon(LtIcons.Fullscreen, "Preview full screen", Modifier.size(20.dp))
                        }
                        FilledTonalIconButton(onClick = { turned = (turned + 1) % 3 }, modifier = Modifier.size(40.dp).testTag("preview_rotate")) {
                            Icon(LtIcons.Rotate, "Rotate", Modifier.size(20.dp))
                        }
                    }
                }
            }
        }
    }
    if (full) FullPreview(source, method, screen, shownPixels, degrees, s.integerScale, realPx) { full = false }
}

/**
 * The sample over the whole screen, black around it, at the size full screen draws the guest: Pixel perfect's whole
 * multiple, the real size where it fits, else fitted. A tap or Back leaves.
 */
@Composable
private fun FullPreview(source: Bitmap, method: ScreenScaling, screen: PreviewScreen, shown: IntSize, degrees: Float, fixed: Int,
                        realPx: Float?, onClose: () -> Unit) {
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black).clickable(onClick = onClose).testTag("preview_full")) {
            val w = constraints.maxWidth.toFloat(); val h = constraints.maxHeight.toFloat()
            val fit = min(w / shown.width, h / shown.height)
            val real = realPx?.let { it * screen.skin.screenWidth / screen.pixels.width }
            val scale = when {
                method == ScreenScaling.PIXEL_PERFECT -> wholeMultiple(shown, w, h, fixed).toFloat()
                real != null && shown.width * real <= w + 0.5f && shown.height * real <= h + 0.5f -> real
                else -> fit
            }
            ScaledPicture(source, method, scale, degrees, Modifier.fillMaxSize())
            Text("Tap to leave", color = Color.White.copy(alpha = 0.7f), style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 32.dp))
        }
    }
}

/**
 * The scale the preview draws a sample at in a [box] of physical pixels, and its one-line caption: what the device
 * view does ("Real size · Smoothed 2.59× · soft edges").
 */
private fun previewScale(method: ScreenScaling, pixels: IntSize, viewScale: Float, view: DeviceViewFit, fixed: Int, box: IntSize): Pair<Float, String> {
    fun times(x: Float) = if (abs(x - x.roundToInt()) < 0.01f) "${x.roundToInt()}×" else "%.2f×".format(x)
    if (method == ScreenScaling.PIXEL_PERFECT) {
        val k = view.whole
        val here = largestMultiple(pixels, box.width.toFloat(), box.height.toFloat()).coerceAtLeast(1)
        val shown = min(k, here)
        val what = when {
            fixed > k -> "$fixed× doesn't fit, so $k×"
            else -> "Pixel perfect $k×"
        }
        val detail = if (shown < k) "showing $shown× here" else "each pixel $k×$k"
        return shown.toFloat() to "$what · $detail"
    }
    val fits = pixels.width * viewScale <= box.width + 0.5f && pixels.height * viewScale <= box.height + 0.5f
    val scale = if (fits) viewScale else min(box.width.toFloat() / pixels.width, box.height.toFloat() / pixels.height)
    val k = crispMultiple(viewScale)
    val real = if (view.real) "Real size · " else if (view.realTooLarge) "Too large for real size · " else ""
    val what = when (method) {
        ScreenScaling.SHARP -> if (abs(viewScale - viewScale.roundToInt()) < 0.01f) "Stretched ${times(viewScale)} · even pixels"
            else "Stretched ${times(viewScale)} · pixels vary in width"
        ScreenScaling.CRISP -> if (k == 1) "Crisp is Smooth below 2× · ${times(viewScale)}" else "$k× crisp · smoothed to ${times(viewScale)}"
        else -> "Smoothed ${times(viewScale)} · soft edges"
    }
    val shrunk = if (fits) "" else " · smaller here"
    return scale to "$real$what$shrunk"
}

/** [source] drawn at [scale] by [method], turned [degrees] (0 or a quarter), centred in the box on whole physical pixels. */
@Composable
private fun ScaledPicture(source: Bitmap, method: ScreenScaling, scale: Float, degrees: Float, modifier: Modifier) {
    val k = if (method == ScreenScaling.CRISP) crispMultiple(scale) else 1
    val image: ImageBitmap = remember(source, k) {
        (if (k > 1) Bitmap.createScaledBitmap(source, source.width * k, source.height * k, false) else source).asImageBitmap()
    }
    val filter = if (method == ScreenScaling.SMOOTH || method == ScreenScaling.CRISP) FilterQuality.Low else FilterQuality.None
    val size = IntSize((source.width * scale).roundToInt(), (source.height * scale).roundToInt())
    Canvas(modifier.clipToBounds()) {
        val at = IntOffset(((this.size.width - size.width) / 2).roundToInt(), ((this.size.height - size.height) / 2).roundToInt())
        rotate(degrees) { drawImage(image, IntOffset.Zero, IntSize(image.width, image.height), at, size, filterQuality = filter) }
    }
}

/** The screen at real size ([pxPerMm]) over 1 cm squares, centred, cut off at the box's edges when larger. */
@Composable
private fun Squares(screen: PreviewScreen, pxPerMm: Float, landscape: Boolean, modifier: Modifier) {
    Canvas(modifier.clipToBounds()) {
        val across = screen.skin.screenWidth * pxPerMm; val down = screen.skin.screenHeight * pxPerMm
        val w = if (landscape) down else across; val h = if (landscape) across else down
        val left = ((size.width - w) / 2).roundToInt().toFloat(); val top = ((size.height - h) / 2).roundToInt().toFloat()
        drawRect(Color(0xFF14161A), Offset(left, top), Size(w, h))
        val cm = 10 * pxPerMm
        var x = left + cm
        while (x < left + w - 0.5f) { drawLine(Color(0xFF7A808A), Offset(x, top), Offset(x, top + h), 2f); x += cm }
        var y = top + cm
        while (y < top + h - 0.5f) { drawLine(Color(0xFF7A808A), Offset(left, y), Offset(left + w, y), 2f); y += cm }
    }
}

private enum class Reference(val label: String) { CARD("Bank card"), RULER("Ruler") }

/**
 * Settings → Screen scaling → Measure this display: the phone's density against a bank card or ruler held to the
 * screen, saved for this phone. Real size and the preview's 1 cm squares use it.
 */
@OptIn(ExperimentalMaterial3Api::class, androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
private fun MeasureDisplayScreen(host: GuestHost, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val s = host.settings
    var draft by rememberSaveable { mutableFloatStateOf(s.pxPerMm) }
    var reference by rememberSaveable { mutableStateOf(Reference.CARD) }
    val clamp = { v: Float -> v.coerceIn(2f, 40f) }
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }, color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack, modifier = Modifier.testTag("measure_back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                Text("Measure this display", style = MaterialTheme.typography.titleLarge)
            }
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    Reference.entries.forEachIndexed { i, option ->
                        SegmentedButton(selected = reference == option, onClick = { reference = option }, shape = SegmentedButtonDefaults.itemShape(i, 2),
                            icon = {}, label = { Text(option.label) })
                    }
                }
                Text(if (reference == Reference.CARD) "Hold a bank or ID card upright against the screen, its corner in the outline's top-left corner. " +
                    "Drag the handle until the outline's bottom edge meets the card's."
                    else "Hold a ruler upright along the marks, its 0 on the top one. Drag the handle until the bottom mark meets the same number on the ruler.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(vertical = 12.dp))
                // The reference at the draft density; its far end follows a drag near it (elsewhere, the hand holding a
                // card would move it).
                val length = if (reference == Reference.CARD) 85.60f else 80f
                val heightPx = length * draft + 60f
                val primary = MaterialTheme.colorScheme.primary
                val ink = MaterialTheme.colorScheme.onSurface
                val d = LocalDensity.current
                Box(Modifier.fillMaxWidth().height(with(d) { heightPx.toDp() }).testTag("measure_reference")) {
                    Canvas(Modifier.fillMaxSize()) {
                        val x0 = 4f; val y0 = 4f
                        if (reference == Reference.CARD) {
                            val w = 53.98f * draft; val h = 85.60f * draft
                            drawRoundRect(primary.copy(alpha = 0.12f), Offset(x0, y0), Size(w, h), CornerRadius(3.18f * draft))
                            drawRoundRect(primary, Offset(x0, y0), Size(w, h), CornerRadius(3.18f * draft), style = Stroke(4f))
                            drawRoundRect(primary, Offset(x0 + w / 2 - 40f, y0 + h - 10f), Size(80f, 20f), CornerRadius(10f))
                        } else {
                            drawLine(ink, Offset(x0, y0), Offset(x0, y0 + length * draft), 3f)
                            for (mm in 0..length.toInt()) {
                                val y = y0 + mm * draft
                                val (long, color) = when { mm % 10 == 0 -> 60f to primary; mm % 5 == 0 -> 38f to ink; else -> 20f to ink.copy(alpha = 0.6f) }
                                drawLine(color, Offset(x0, y), Offset(x0 + long, y), if (mm % 10 == 0) 4f else 2f)
                            }
                            drawRoundRect(primary, Offset(x0, y0 + length * draft - 10f), Size(80f, 20f), CornerRadius(10f))
                        }
                    }
                    Box(Modifier.offset(y = with(d) { (4f + length * draft - 90f).toDp() }).fillMaxWidth().height(with(d) { 180f.toDp() })
                        .testTag("measure_handle")
                        .pointerInput(reference) { detectVerticalDragGestures { change, dy -> change.consume(); draft = clamp(draft + dy / length) } })
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Density", style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
                    IconButton(onClick = { draft = clamp(draft - 1f / length) }) { Text("−", style = MaterialTheme.typography.titleLarge) }
                    Text("%.1f pixels/inch".format(draft * 25.4f), style = MaterialTheme.typography.bodyLarge)
                    IconButton(onClick = { draft = clamp(draft + 1f / length) }) { Icon(Icons.Filled.Add, "Larger") }
                }
                Text("The system reports %.1f pixels per inch.".format(s.systemPxPerMm * 25.4f), style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
                Row(Modifier.padding(vertical = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                    Button(onClick = { s.displayPxPerMm = draft; onBack() }, modifier = Modifier.testTag("measure_save")) { Text("Save") }
                    Spacer(Modifier.width(8.dp))
                    TextButton(onClick = { s.displayPxPerMm = 0f; draft = s.systemPxPerMm }) { Text("Use the system's value") }
                }
            }
        }
    }
}
