package org.lighttouch.android

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
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

/**
 * Pixel perfect's multiple for a screen of [pixels] in a room of so many physical pixels: [fixed] (0 for the largest)
 * where it fits, else the largest that does; 0 when not even 1x fits.
 */
internal fun wholeMultiple(pixels: IntSize, roomWidth: Float, roomHeight: Float, fixed: Int): Int {
    val largest = (12 downTo 1).firstOrNull { pixels.width * it <= roomWidth + 0.5f && pixels.height * it <= roomHeight + 0.5f } ?: 0
    return if (fixed in 1..largest) fixed else largest
}

/**
 * Crisp's prescale, as the presenter makes it: the largest whole multiple that fits (1 below 2x, as Smooth; the
 * desktop app, which prescales on its GPU's behalf, uses 2 there).
 */
internal fun crispMultiple(scale: Float): Int = floor(scale + 0.01f).toInt().coerceIn(1, 8)

/** The screens the preview can show. */
private enum class PreviewScreen(val label: String, val pixels: IntSize, val skin: DeviceSkin) {
    CLASSIC("iPhone, iPod touch · 320 × 480", IntSize(320, 480), DeviceSkin.ipodTouch2),
    RETINA("Retina · 640 × 960", IntSize(640, 960), DeviceSkin.ipodTouch4),
    TALL("iPhone 5 · 640 × 1136", IntSize(640, 1136), DeviceSkin.iphone5),
    IPAD("iPad · 1024 × 768", IntSize(768, 1024), DeviceSkin.ipad);

    companion object {
        fun forBoard(board: String?) = screenPixels(board).let { p -> entries.first { it.pixels == p } }
    }
}

/** The preview's sample pictures (art/preview/scaling, drawn by tools/scaling-samples/make.py). */
private enum class PreviewSample(val label: String, val file: String) { PIXEL_ART("Pixel art", "pixel"), GAME("3D game", "game"), HOME("Home screen", "home") }

@OptIn(ExperimentalMaterial3Api::class, androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun ScreenScalingScreen(host: GuestHost, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val s = host.settings
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
                if (!previewing) SettingsGroup("Method") { Methods(host) } else Spacer(Modifier.height(12.dp))
                if (previewing) Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainer)
                    .padding(vertical = 4.dp)) { Methods(host, compact = true) }
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
    for (method in ScreenScaling.entries) {
        val selected = s.scaling == method
        Row(Modifier.fillMaxWidth().selectable(selected, role = Role.RadioButton) { s.scaling = method; host.applySettings() }
            .padding(horizontal = 12.dp, vertical = if (compact) 4.dp else 8.dp).testTag("scaling_" + method.name.lowercase()),
            verticalAlignment = Alignment.CenterVertically) {
            RadioButton(selected, onClick = null)
            Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(method.label, style = MaterialTheme.typography.bodyLarge)
                if (!compact) Text(method.about, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        if (method == ScreenScaling.PIXEL_PERFECT && selected) {
            val sizes = listOf(0 to "Auto", 1 to "1×", 2 to "2×", 3 to "3×", 4 to "4×")
            SingleChoiceSegmentedButtonRow(Modifier.padding(start = 52.dp, end = 12.dp, bottom = 8.dp).testTag("scaling_size")) {
                sizes.forEachIndexed { i, (k, label) ->
                    SegmentedButton(selected = s.integerScale == k, onClick = { s.integerScale = k }, shape = SegmentedButtonDefaults.itemShape(i, sizes.size),
                        icon = {}, label = { Text(label, maxLines = 1, style = MaterialTheme.typography.labelMedium) })
                }
            }
        }
    }
}

/**
 * A sample picture or squares in a screen's own pixels, drawn the way the presenter will: bilinear (Smooth), nearest
 * (Sharp), Crisp's whole-number prescale then bilinear, or Pixel perfect's whole multiple with a border.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun ScalingPreview(host: GuestHost, modifier: Modifier) {
    val context = LocalContext.current
    var screen by rememberSaveable { mutableStateOf(PreviewScreen.forBoard(host.selected?.board)) }
    var sample by rememberSaveable { mutableStateOf(PreviewSample.PIXEL_ART) }
    var squares by rememberSaveable { mutableStateOf(false) }
    var menu by remember { mutableStateOf(false) }
    val method = host.settings.scaling
    val source = remember(screen, sample, squares) {
        if (squares) squares(screen) else try {
            context.assets.open("scaling/${sample.file}-${screen.pixels.width}x${screen.pixels.height}.png").use { BitmapFactory.decodeStream(it) }
        } catch (_: Exception) { squares(screen) }
    }
    Column(modifier) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Box {
                AssistChip(onClick = { menu = true }, label = { Text(screen.label) }, trailingIcon = { Icon(Icons.Filled.ArrowDropDown, null) },
                    modifier = Modifier.testTag("preview_screen"))
                DropdownMenu(menu, onDismissRequest = { menu = false }) {
                    for (option in PreviewScreen.entries) DropdownMenuItem(text = { Text(option.label) }, onClick = { screen = option; menu = false })
                }
            }
        }
        val views = listOf(false to "Samples", true to "1 cm squares")
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().testTag("preview_view")) {
            views.forEachIndexed { i, (value, label) ->
                SegmentedButton(selected = squares == value, onClick = { squares = value }, shape = SegmentedButtonDefaults.itemShape(i, views.size),
                    icon = {}, label = { Text(label, maxLines = 1, style = MaterialTheme.typography.labelMedium) })
            }
        }
        // Wraps on a narrow phone rather than running off the edge.
        if (!squares) FlowRow(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for (option in PreviewSample.entries)
                FilterChip(sample == option, onClick = { sample = option }, label = { Text(option.label, maxLines = 1) })
        }
        Spacer(Modifier.height(4.dp))
        var caption by remember { mutableStateOf("") }
        Box(Modifier.fillMaxWidth().weight(1f).clip(RoundedCornerShape(16.dp)).background(Color(0xFF0E0F12)).testTag("preview_picture")) {
            ScaledPicture(source, method, host.settings.integerScale, Modifier.fillMaxSize().padding(8.dp)) { caption = it }
        }
        Text(if (squares) "Squares are 1 cm on the real device. $caption" else caption, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(start = 4.dp, top = 6.dp))
    }
}

@Composable
private fun ScaledPicture(source: Bitmap, method: ScreenScaling, fixed: Int, modifier: Modifier, onCaption: (String) -> Unit) {
    val pixels = IntSize(source.width, source.height)
    BoxWithConstraints(modifier) {
        val room = with(androidx.compose.ui.platform.LocalDensity.current) { maxWidth.toPx() to maxHeight.toPx() }
        val fit = min(room.first / pixels.width, room.second / pixels.height)
        val whole = if (method == ScreenScaling.PIXEL_PERFECT) wholeMultiple(pixels, room.first, room.second, fixed) else 0
        val scale = if (whole > 0) whole.toFloat() else fit
        val k = if (method == ScreenScaling.CRISP) crispMultiple(scale) else 1
        val image: ImageBitmap = remember(source, k) {
            (if (k > 1) Bitmap.createScaledBitmap(source, source.width * k, source.height * k, false) else source).asImageBitmap()
        }
        val filter = if (method == ScreenScaling.SMOOTH || method == ScreenScaling.CRISP) FilterQuality.Low else FilterQuality.None
        val size = IntSize((pixels.width * scale).roundToInt(), (pixels.height * scale).roundToInt())
        LaunchedEffect(scale) {
            onCaption(if (scale == floor(scale)) "Shown at ${scale.toInt()}× its pixels" else "Shown at ${"%.2f".format(scale)}× its pixels")
        }
        Canvas(Modifier.fillMaxSize()) {
            val at = IntOffset(((this.size.width - size.width) / 2).roundToInt(), ((this.size.height - size.height) / 2).roundToInt())
            drawImage(image, IntOffset.Zero, IntSize(image.width, image.height), at, size, filterQuality = filter)
        }
    }
}

/** 1 cm squares of the real device in its own pixels: 1-pixel lines, which show how each method draws fine detail. */
private fun squares(screen: PreviewScreen): Bitmap {
    val (w, h) = screen.pixels.width to screen.pixels.height
    val perCm = w / (screen.skin.screenWidth / 10f)
    val line = 0xFF5A5E66.toInt(); val back = 0xFF14161A.toInt()
    val columns = (1 until w).filter { x -> floor(x / perCm) != floor((x - 1) / perCm) }.toSet()
    val rows = (1 until h).filter { y -> floor(y / perCm) != floor((y - 1) / perCm) }.toSet()
    val data = IntArray(w * h) { i -> if ((i % w) in columns || (i / w) in rows) line else back }
    return Bitmap.createBitmap(data, w, h, Bitmap.Config.ARGB_8888)
}
