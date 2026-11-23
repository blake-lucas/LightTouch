package org.lighttouch.android

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties

/**
 * Moving a device to another phone or computer (the core's Transfer.cpp): a device file is a zip with the device's
 * system, data and settings, which LightTouch on Android, Linux, Windows or macOS imports as a new device.
 */
class TransferState(val title: String, val exporting: Boolean) {
    var phase by mutableStateOf("")
    var done by mutableLongStateOf(0L)
    var total by mutableLongStateOf(0L)
    var fraction by mutableFloatStateOf(-1f)
    var cancelling by mutableStateOf(false)

    companion object {
        /** "iPod touch 4G 6.1.6.lighttouch.zip": the name an export offers, safe on every file system. */
        fun fileName(deviceName: String): String {
            val safe = deviceName.map { if (it.isISOControl() || it in "/\\:*?\"<>|") '-' else it }.joinToString("").trim().trimEnd('.').trim()
            return "${safe.ifEmpty { "Device" }}.lighttouch.zip"
        }
        /** The core's phase, in words. */
        fun phaseText(phase: String) = when (phase) {
            "exporting" -> "Exporting"; "importing" -> "Importing"; "finishing" -> "Finishing up"; else -> "Starting"
        }
        fun size(bytes: Long) = if (bytes >= 1_000_000_000) "%.1f GB".format(bytes / 1e9) else "%.0f MB".format(maxOf(bytes / 1e6, if (bytes > 0) 1.0 else 0.0))
    }
}

/** The running export or import: a bar, what it is doing, and Cancel. Nothing else can be done until it ends. */
@Composable
internal fun TransferDialog(state: TransferState, onCancel: () -> Unit) {
    AlertDialog(onDismissRequest = {}, properties = DialogProperties(dismissOnBackPress = false, dismissOnClickOutside = false),
        modifier = DialogTags.testTag("transfer_dialog"),
        icon = { Icon(if (state.exporting) LtIcons.Upload else LtIcons.Download, null) },
        title = { Text(state.title, style = MaterialTheme.typography.titleMedium) },
        text = {
            Column {
                if (state.fraction >= 0f) LinearProgressIndicator(progress = { state.fraction }, modifier = Modifier.fillMaxWidth().testTag("transfer_progress"))
                else LinearProgressIndicator(Modifier.fillMaxWidth().testTag("transfer_progress"))
                Spacer(Modifier.height(12.dp))
                val status = when {
                    state.cancelling -> "Cancelling…"
                    state.total > 0 -> "${TransferState.phaseText(state.phase)} · ${TransferState.size(state.done)} of ${TransferState.size(state.total)}"
                    else -> "${TransferState.phaseText(state.phase)}…"
                }
                Text(status, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onCancel, enabled = !state.cancelling, modifier = Modifier.testTag("cancel_transfer")) { Text("Cancel") } })
}
