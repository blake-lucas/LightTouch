package org.lighttouch.android

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

enum class ThemeMode(val label: String) { SYSTEM("System"), DARK("Dark"), LIGHT("Light") }
enum class OverlayLevel(val label: String) { OFF("Off"), FPS("FPS"), COMPACT("Basic"), FULL("Full") }
enum class OverlayCorner(val label: String, val glyph: String) { TOP_LEFT("Top left", "↖"), TOP_RIGHT("Top right", "↗"), BOTTOM_LEFT("Bottom left", "↙"), BOTTOM_RIGHT("Bottom right", "↘") }
enum class OverlayGraph(val label: String) { FRAME_TIME("Frame time"), FPS("Frame rate") }
/**
 * Settings → Screen scaling. [native] is the presenter's PresentScale (lighttouch_jni.cpp): 0 posts a guest-sized
 * buffer the display hardware scales, 1 a whole-number multiple it brings to size (Pixel perfect sizes the view to
 * that multiple, so nothing is scaled), 2 draws nearest neighbour at the view's size on the CPU.
 */
enum class ScreenScaling(val label: String, val about: String, val native: Int) {
    SMOOTH("Smooth", "Blends pixels: soft, with no jagged edges", 0),
    SHARP("Sharp", "Hard-edged pixels, some a little wider than others", 2),
    CRISP("Crisp", "Sharp pixels, evened out with a hint of blending", 1),
    PIXEL_PERFECT("Pixel perfect", "Whole-number sizes only, with a border", 1),
}

/** Settings → Screen scaling → Real size: which devices show at the real device's size. */
enum class RealSize(val label: String) { OFF("Off"), IPADS("iPads"), ALL("All") }

/** User preferences, as Compose state backed by SharedPreferences. Read on the main thread. */
class AppSettings(private val context: Context) {
    private val networkState = mutableStateOf(NetworkConfiguration.load(context))
    var network: NetworkConfiguration
        get() = networkState.value
        set(value) { NetworkConfiguration.save(context, value); networkState.value = value }

    private val prefs = context.getSharedPreferences("ui", Context.MODE_PRIVATE)

    private inline fun <reified E : Enum<E>> enumPref(key: String, default: E) =
        mutableStateOf(prefs.getString(key, null)?.let { name -> enumValues<E>().firstOrNull { it.name == name } } ?: default)

    private val themeState = enumPref("theme", ThemeMode.SYSTEM)
    var theme: ThemeMode get() = themeState.value; set(v) { themeState.value = v; prefs.edit().putString("theme", v.name).apply() }

    private val tiltState = mutableStateOf(prefs.getBoolean("tilt", true))
    var tilt: Boolean get() = tiltState.value; set(v) { tiltState.value = v; prefs.edit().putBoolean("tilt", v).apply() }

    private val volumeKeysState = mutableStateOf(prefs.getBoolean("volumeKeys", false))
    var volumeKeys: Boolean get() = volumeKeysState.value; set(v) { volumeKeysState.value = v; prefs.edit().putBoolean("volumeKeys", v).apply() }

    /* Headphones in the device's jack: iOS skips its speaker processing (mono, EQ, limiter). Read at start. */
    private val headphonesState = mutableStateOf(prefs.getBoolean("headphones", true))
    var headphones: Boolean get() = headphonesState.value; set(v) { headphonesState.value = v; prefs.edit().putBoolean("headphones", v).apply() }

    /* iOS apps that record hear the phone's microphone (needs RECORD_AUDIO; MainActivity asks when it is turned on). */
    private val microphoneState = mutableStateOf(prefs.getBoolean("microphone", false))
    var microphone: Boolean get() = microphoneState.value; set(v) { microphoneState.value = v; prefs.edit().putBoolean("microphone", v).apply() }

    private val hapticsState = mutableStateOf(prefs.getBoolean("haptics", true))
    var haptics: Boolean get() = hapticsState.value; set(v) { hapticsState.value = v; prefs.edit().putBoolean("haptics", v).apply() }

    /* The phone vibrates while an emulated iPhone runs its vibration motor (EngineService's guest-vibrator). */
    private val vibrationState = mutableStateOf(prefs.getBoolean("vibration", true))
    var vibration: Boolean get() = vibrationState.value; set(v) { vibrationState.value = v; prefs.edit().putBoolean("vibration", v).apply() }

    private val autoResumeState = mutableStateOf(prefs.getBoolean("autoResume", true))
    var autoResume: Boolean get() = autoResumeState.value; set(v) { autoResumeState.value = v; prefs.edit().putBoolean("autoResume", v).apply() }

    /* Skip the engine's code-cache flushes when an iOS app opens (LT_FLUSH_ON_APP/LT_FLUSH_SETTLE). Read at start. */
    private val fewerAppPausesState = mutableStateOf(prefs.getBoolean("fewerAppPauses", false))
    var fewerAppPauses: Boolean get() = fewerAppPausesState.value; set(v) { fewerAppPausesState.value = v; prefs.edit().putBoolean("fewerAppPauses", v).apply() }

    // "screenScaling" since 0.0.15. The old "scaling" key's Sharp was a whole-number multiple filtered to fit by the
    // display hardware: Crisp now.
    private val scalingState = mutableStateOf(prefs.getString("screenScaling", null)?.let { name -> ScreenScaling.entries.firstOrNull { it.name == name } }
        ?: if (prefs.getString("scaling", null) == "SHARP") ScreenScaling.CRISP else ScreenScaling.SMOOTH)
    var scaling: ScreenScaling get() = scalingState.value; set(v) { scalingState.value = v; prefs.edit().putString("screenScaling", v.name).apply() }

    private val realSizeState = enumPref("realSize", RealSize.OFF)
    var realSize: RealSize get() = realSizeState.value; set(v) { realSizeState.value = v; prefs.edit().putString("realSize", v.name).apply() }

    /* This phone's measured pixels per millimetre (Measure this display); 0 uses what the system reports. */
    private val displayPxPerMmState = mutableStateOf(prefs.getFloat("displayPxPerMm", 0f))
    var displayPxPerMm: Float get() = displayPxPerMmState.value; set(v) { displayPxPerMmState.value = v; prefs.edit().putFloat("displayPxPerMm", v).apply() }
    /** The density the system reports, in pixels per millimetre (many phones are a few percent off). */
    val systemPxPerMm: Float get() = context.resources.displayMetrics.let { (it.xdpi + it.ydpi) / 2f / 25.4f }
    val pxPerMm: Float get() = displayPxPerMm.takeIf { it > 0f } ?: systemPxPerMm
    /** Pixels per millimetre when [board] shows at its real size; null when it doesn't (Pixel perfect sets its own size). */
    fun realPxPerMm(board: String?): Float? = when {
        scaling == ScreenScaling.PIXEL_PERFECT -> null
        realSize == RealSize.ALL || (realSize == RealSize.IPADS && board?.startsWith("k48") == true) -> pxPerMm
        else -> null
    }

    /* Pixel perfect's multiple; 0 is the largest that fits. */
    private val integerScaleState = mutableStateOf(prefs.getInt("integerScale", 0))
    var integerScale: Int get() = integerScaleState.value; set(v) { integerScaleState.value = v; prefs.edit().putInt("integerScale", v).apply() }

    private val overlayState = enumPref("overlay", OverlayLevel.OFF)
    var overlay: OverlayLevel get() = overlayState.value; set(v) { overlayState.value = v; prefs.edit().putString("overlay", v.name).apply() }

    private val overlayCornerState = enumPref("overlayCorner", OverlayCorner.TOP_LEFT)
    var overlayCorner: OverlayCorner get() = overlayCornerState.value; set(v) { overlayCornerState.value = v; prefs.edit().putString("overlayCorner", v.name).apply() }

    private val overlayGraphState = enumPref("overlayGraph", OverlayGraph.FRAME_TIME)
    var overlayGraph: OverlayGraph get() = overlayGraphState.value; set(v) { overlayGraphState.value = v; prefs.edit().putString("overlayGraph", v.name).apply() }

    private val overlayOpacityState = mutableStateOf(prefs.getFloat("overlayOpacity", 0.55f))
    var overlayOpacity: Float get() = overlayOpacityState.value; set(v) { overlayOpacityState.value = v; prefs.edit().putFloat("overlayOpacity", v).apply() }

    private val overlayInDeviceViewState = mutableStateOf(prefs.getBoolean("overlayInDeviceView", true))
    var overlayInDeviceView: Boolean get() = overlayInDeviceViewState.value; set(v) { overlayInDeviceViewState.value = v; prefs.edit().putBoolean("overlayInDeviceView", v).apply() }

    /* Screenshot in the device page's dock: off by default, also for phones that had it before (full screen's menu keeps it). */
    private val screenshotButtonState = mutableStateOf(prefs.getBoolean("screenshotButton", false))
    var screenshotButton: Boolean get() = screenshotButtonState.value; set(v) { screenshotButtonState.value = v; prefs.edit().putBoolean("screenshotButton", v).apply() }
    /* The Carrier button on every device with a modem (each device's own setting in 0.0.15's test builds). */
    private val carrierButtonState = mutableStateOf(prefs.getBoolean("carrierButton", false))
    var carrierButton: Boolean get() = carrierButtonState.value; set(v) { carrierButtonState.value = v; prefs.edit().putBoolean("carrierButton", v).apply() }
    /* The States button (save states of the device on screen): on by default. */
    private val statesButtonState = mutableStateOf(prefs.getBoolean("statesButton", true))
    var statesButton: Boolean get() = statesButtonState.value; set(v) { statesButtonState.value = v; prefs.edit().putBoolean("statesButton", v).apply() }
    /* The device page's dock tucked away to a tab at the right edge, so it doesn't cover the device. */
    private val dockHiddenState = mutableStateOf(prefs.getBoolean("dockHidden", false))
    var dockHidden: Boolean get() = dockHiddenState.value; set(v) { dockHiddenState.value = v; prefs.edit().putBoolean("dockHidden", v).apply() }
    private val hitchButtonState = mutableStateOf(prefs.getBoolean("hitchButton", false))
    var hitchButton: Boolean get() = hitchButtonState.value; set(v) { hitchButtonState.value = v; prefs.edit().putBoolean("hitchButton", v).apply() }

    private val detailedTimingState = mutableStateOf(prefs.getBoolean("detailedTiming", false))
    var detailedTiming: Boolean get() = detailedTimingState.value; set(v) { detailedTimingState.value = v; prefs.edit().putBoolean("detailedTiming", v).apply() }
}
