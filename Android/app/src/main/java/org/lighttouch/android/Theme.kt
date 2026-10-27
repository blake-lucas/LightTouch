package org.lighttouch.android

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

internal object LtColors {
    val Accent = Color(0xFF71DCC8)
    val AccentDeep = Color(0xFF0F7F6F)
    val Danger = Color(0xFFFF6B6B)
    val Warning = Color(0xFFFFC56B)
    // Overlay colours follow MangoHud's convention: one hue per subsystem.
    val OverlayFps = Color(0xFFFFB86B)
    val OverlayCpu = Color(0xFF6BB8FF)
    val OverlayGpu = Color(0xFF7CE38B)
    val OverlayMem = Color(0xFFD59BFF)
    val OverlayDisk = Color(0xFFFF8FA8)
    val OverlayLabel = Color(0xFFB8BCC4)
}

private val Dark = darkColorScheme(
    primary = LtColors.Accent, onPrimary = Color(0xFF00382F),
    primaryContainer = Color(0xFF12483F), onPrimaryContainer = Color(0xFFB6F3E6),
    secondary = Color(0xFFB4C8C3), onSecondary = Color(0xFF1F3531),
    secondaryContainer = Color(0xFF2A3331), onSecondaryContainer = Color(0xFFD4E5E0),
    background = Color(0xFF0D0F10), onBackground = Color(0xFFE4E6E7),
    surface = Color(0xFF0D0F10), onSurface = Color(0xFFE4E6E7),
    surfaceVariant = Color(0xFF22272A), onSurfaceVariant = Color(0xFF9AA2A6),
    surfaceContainerLowest = Color(0xFF08090A), surfaceContainerLow = Color(0xFF141718),
    surfaceContainer = Color(0xFF181B1D), surfaceContainerHigh = Color(0xFF1F2326),
    surfaceContainerHighest = Color(0xFF282D30),
    outline = Color(0xFF3A4145), outlineVariant = Color(0xFF2A3033),
    error = LtColors.Danger, onError = Color(0xFF3B0A0A),
    errorContainer = Color(0xFF4A1616), onErrorContainer = Color(0xFFFFDAD6),
)

private val Light = lightColorScheme(
    primary = LtColors.AccentDeep, onPrimary = Color.White,
    primaryContainer = Color(0xFFC9F1E8), onPrimaryContainer = Color(0xFF00201B),
    secondary = Color(0xFF4A635E), onSecondary = Color.White,
    secondaryContainer = Color(0xFFDCE6E3), onSecondaryContainer = Color(0xFF14201D),
    background = Color(0xFFF3F5F5), onBackground = Color(0xFF151819),
    surface = Color(0xFFF3F5F5), onSurface = Color(0xFF151819),
    surfaceVariant = Color(0xFFE1E5E6), onSurfaceVariant = Color(0xFF5A6366),
    surfaceContainerLowest = Color.White, surfaceContainerLow = Color(0xFFF8FAFA),
    surfaceContainer = Color(0xFFFFFFFF), surfaceContainerHigh = Color(0xFFEDF0F0),
    surfaceContainerHighest = Color(0xFFE5E9E9),
    outline = Color(0xFFC3CACC), outlineVariant = Color(0xFFDDE2E3),
    error = Color(0xFFC62828), onError = Color.White,
    errorContainer = Color(0xFFFFDAD6), onErrorContainer = Color(0xFF410002),
)

private val Type = Typography().let { base ->
    base.copy(
        headlineLarge = base.headlineLarge.copy(fontWeight = FontWeight.SemiBold, letterSpacing = (-0.5).sp),
        headlineMedium = base.headlineMedium.copy(fontWeight = FontWeight.SemiBold, letterSpacing = (-0.25).sp),
        headlineSmall = base.headlineSmall.copy(fontWeight = FontWeight.SemiBold),
        titleLarge = base.titleLarge.copy(fontWeight = FontWeight.SemiBold),
        titleMedium = base.titleMedium.copy(fontWeight = FontWeight.SemiBold),
        labelLarge = base.labelLarge.copy(fontWeight = FontWeight.SemiBold),
    )
}

internal val MonoStyle = TextStyle(fontFamily = FontFamily.Monospace, fontSize = 11.sp, lineHeight = 14.sp)

/** Full screen is always dark: its menus sit on black around the guest. */
@Composable
internal fun DarkTheme(content: @Composable () -> Unit) = MaterialTheme(colorScheme = Dark, typography = Type,
    shapes = MaterialTheme.shapes) { OnBackground(content) }

/** Screens drawn straight on the background, outside a Surface, still get readable text and icons. */
@Composable
private fun OnBackground(content: @Composable () -> Unit) =
    CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onBackground, content = content)

@Composable
internal fun LightTouchTheme(mode: ThemeMode, content: @Composable () -> Unit) {
    val dark = when (mode) { ThemeMode.SYSTEM -> isSystemInDarkTheme(); ThemeMode.DARK -> true; ThemeMode.LIGHT -> false }
    MaterialTheme(colorScheme = if (dark) Dark else Light, typography = Type,
        shapes = Shapes(small = RoundedCornerShape(10.dp), medium = RoundedCornerShape(16.dp), large = RoundedCornerShape(24.dp))) { OnBackground(content) }
}
