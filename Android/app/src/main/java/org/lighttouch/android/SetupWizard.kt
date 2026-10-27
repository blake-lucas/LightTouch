package org.lighttouch.android

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

internal enum class SetupStep { WELCOME, DONE }

/** Wizard state; the activity owns it so a finished preparation can show "ready". */
internal class SetupState {
    var step by mutableStateOf(SetupStep.WELCOME)
    var created by mutableStateOf<String?>(null)
    /** How long each stage of a preparation took ("stage\tseconds" lines), shown folded on the finished screen. */
    var timings by mutableStateOf("")
    fun reset() {
        step = SetupStep.WELCOME; created = null; timings = ""
    }
}

/**
 * First launch: a welcome that leads into preparing a device from an IPSW on the phone (PrepareScreen), and the
 * "ready" screen a preparation ends on, offering to start the device. Guest tools come with LightTouch.
 */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun SetupWizard(state: SetupState, canCancel: Boolean, onPrepare: () -> Unit,
                         onFinish: (start: Boolean) -> Unit, onCancel: () -> Unit) {
    BackHandler {
        when {
            state.step == SetupStep.DONE -> onFinish(false)
            canCancel -> onCancel()
        }
    }
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }.testTag("setup"), color = MaterialTheme.colorScheme.background) {
        Box(Modifier.fillMaxSize().background(Brush.verticalGradient(listOf(MaterialTheme.colorScheme.primary.copy(alpha = 0.10f), Color.Transparent))))
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            if (canCancel) Row(Modifier.fillMaxWidth().padding(4.dp)) {
                IconButton(onClick = { if (state.step == SetupStep.DONE) onFinish(false) else onCancel() }) { Icon(Icons.Filled.Close, "Close") }
            }
            AnimatedContent(state.step, Modifier.weight(1f), label = "setup-step") { step ->
                when (step) {
                    SetupStep.WELCOME -> Welcome(onPrepare)
                    SetupStep.DONE -> Done(state.created ?: "Your device", state.timings, onFinish)
                }
            }
        }
    }
}

@Composable
private fun Welcome(onPrepare: () -> Unit) {
    Column(Modifier.fillMaxSize().padding(horizontal = 28.dp), verticalArrangement = Arrangement.Center) {
        Box(Modifier.size(72.dp).background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(20.dp)), contentAlignment = Alignment.Center) {
            Icon(LtIcons.Phone, null, Modifier.size(38.dp), tint = MaterialTheme.colorScheme.onPrimaryContainer)
        }
        Spacer(Modifier.height(28.dp))
        Text("LightTouch", style = MaterialTheme.typography.headlineLarge)
        Spacer(Modifier.height(8.dp))
        Text("Classic iPod touch and iPhone software, running on your phone.", style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.height(36.dp))
        Text("LightTouch sets up a device for you, right on this phone:", style = MaterialTheme.typography.bodyMedium)
        Spacer(Modifier.height(16.dp))
        Need(LtIcons.Phone, "Choose a device and iOS version", "The iPod touch 2G on iOS 3.1.3 is the easiest place to start")
        Need(LtIcons.Download, "LightTouch downloads it", "iOS comes from Apple, or the Internet Archive when Apple no longer has it")
        Need(LtIcons.Layers, "And prepares it", "A minute or a few, with 1.5 to 5 GB free")
        Spacer(Modifier.height(40.dp))
        Button(onClick = onPrepare, modifier = Modifier.align(Alignment.CenterHorizontally).height(52.dp).testTag("setup_start"), contentPadding = PaddingValues(horizontal = 32.dp)) {
            Text("Get started", style = MaterialTheme.typography.titleMedium)
        }
    }
}

@Composable
private fun Need(icon: ImageVector, title: String, detail: String) {
    Row(Modifier.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(icon, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.primary)
        Spacer(Modifier.width(14.dp))
        Column {
            Text(title, style = MaterialTheme.typography.titleSmall)
            Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Done(name: String, timings: String, onFinish: (Boolean) -> Unit) {
    Column(Modifier.fillMaxSize().padding(horizontal = 28.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        Icon(Icons.Filled.CheckCircle, null, Modifier.size(72.dp), tint = MaterialTheme.colorScheme.primary)
        Spacer(Modifier.height(24.dp))
        Text("$name is ready", style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center)
        Spacer(Modifier.height(8.dp))
        Text("The first start takes longer while iOS sets itself up.", style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
        Spacer(Modifier.height(36.dp))
        Button(onClick = { onFinish(true) }, modifier = Modifier.height(52.dp).testTag("setup_launch"), contentPadding = PaddingValues(start = 24.dp, end = 32.dp)) {
            Icon(LtIcons.Play, null, Modifier.size(20.dp)); Spacer(Modifier.width(10.dp)); Text("Start it now", style = MaterialTheme.typography.titleMedium)
        }
        TextButton(onClick = { onFinish(false) }, modifier = Modifier.padding(top = 8.dp)) { Text("Not now") }
        if (timings.isNotEmpty()) StageTimes(timings)
    }
}

/** A preparation's stages and their seconds, folded until asked for (for debugging slow preparations). */
@Composable
private fun StageTimes(timings: String) {
    var open by remember { mutableStateOf(false) }
    val rows = timings.lines().mapNotNull { line -> line.split('\t').takeIf { it.size == 2 }?.let { it[0] to it[1] } }
    TextButton(onClick = { open = !open }, modifier = Modifier.padding(top = 24.dp).testTag("setup_details")) {
        Text(if (open) "Hide details" else "Details")
    }
    if (open) Column(Modifier.fillMaxWidth().padding(top = 4.dp)) {
        for ((stage, seconds) in rows + ("Total" to "%.1f".format(rows.sumOf { it.second.toDoubleOrNull() ?: 0.0 }))) {
            Row(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
                Text(stage, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text("$seconds s", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}
