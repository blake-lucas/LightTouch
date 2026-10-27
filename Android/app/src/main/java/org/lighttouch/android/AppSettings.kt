package org.lighttouch.android

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

enum class ThemeMode(val label: String) { SYSTEM("System"), DARK("Dark"), LIGHT("Light") }
enum class OverlayLevel(val label: String) { OFF("Off"), FPS("FPS"), COMPACT("Basic"), FULL("Full") }
enum class OverlayCorner(val label: String, val glyph: String) { TOP_LEFT("Top left", "↖"), TOP_RIGHT("Top right", "↗"), BOTTOM_LEFT("Bottom left", "↙"), BOTTOM_RIGHT("Bottom right", "↘") }
enum class OverlayGraph(val label: String) { FRAME_TIME("Frame time"), FPS("Frame rate") }
/** Matches the presenter's PresentScale (lighttouch_jni.cpp). */
enum class ScreenScaling(val label: String, val native: Int) { SMOOTH("Smooth", 0), SHARP("Sharp", 1) }

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

    private val hapticsState = mutableStateOf(prefs.getBoolean("haptics", true))
    var haptics: Boolean get() = hapticsState.value; set(v) { hapticsState.value = v; prefs.edit().putBoolean("haptics", v).apply() }

    private val autoResumeState = mutableStateOf(prefs.getBoolean("autoResume", true))
    var autoResume: Boolean get() = autoResumeState.value; set(v) { autoResumeState.value = v; prefs.edit().putBoolean("autoResume", v).apply() }

    private val scalingState = enumPref("scaling", ScreenScaling.SMOOTH)
    var scaling: ScreenScaling get() = scalingState.value; set(v) { scalingState.value = v; prefs.edit().putString("scaling", v.name).apply() }

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

    private val hitchButtonState = mutableStateOf(prefs.getBoolean("hitchButton", false))
    var hitchButton: Boolean get() = hitchButtonState.value; set(v) { hitchButtonState.value = v; prefs.edit().putBoolean("hitchButton", v).apply() }

    private val detailedTimingState = mutableStateOf(prefs.getBoolean("detailedTiming", false))
    var detailedTiming: Boolean get() = detailedTimingState.value; set(v) { detailedTimingState.value = v; prefs.edit().putBoolean("detailedTiming", v).apply() }
}
