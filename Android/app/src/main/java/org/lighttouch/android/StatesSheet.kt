package org.lighttouch.android

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.text.DateFormat
import java.util.Date

/** The dock's States button (a floppy disk): its sheet saves the running device and loads, renames or deletes states. */
@Composable
internal fun StatesDockButton(host: GuestHost, modifier: Modifier = Modifier) {
    var open by remember { mutableStateOf(false) }
    DockButton(LtIcons.Save, "States", { open = true }, modifier, enabled = host.selected != null && !host.busy, tag = "states_button")
    if (open) StatesSheet(host) { open = false }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun StatesSheet(host: GuestHost, onDismiss: () -> Unit) {
    val states = host.saveStates
    val device = host.selected
    LaunchedEffect(device?.id) { states.refresh(device?.id, force = true) }
    var loading by remember { mutableStateOf<SaveStateSummary?>(null) }
    var deleting by remember { mutableStateOf<SaveStateSummary?>(null) }
    var renaming by remember { mutableStateOf<SaveStateSummary?>(null) }
    val running = host.shownStatus in listOf("running", "paused")
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), modifier = DialogTags.testTag("states_sheet")) {
        Column(Modifier.padding(horizontal = 20.dp).padding(bottom = 24.dp)) {
            Text("States", style = MaterialTheme.typography.titleMedium)
            Text(device?.name ?: "", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(12.dp))
            states.job?.let { job ->
                Text("${job.label}… ${job.fraction?.let { "${(it * 100).toInt()}%" } ?: ""}", style = MaterialTheme.typography.bodySmall)
                Spacer(Modifier.height(6.dp))
                val fraction = job.fraction
                if (fraction != null) LinearProgressIndicator(progress = { fraction }, modifier = Modifier.fillMaxWidth())
                else LinearProgressIndicator(Modifier.fillMaxWidth())
                Spacer(Modifier.height(12.dp))
            }
            Button(onClick = host::saveState, enabled = running && states.job == null, modifier = Modifier.testTag("save_state")) {
                Icon(LtIcons.Save, null, Modifier.size(18.dp)); Spacer(Modifier.width(8.dp)); Text("Save state")
            }
            if (!running) Text("Start the device to save its state.", style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 6.dp))
            Spacer(Modifier.height(14.dp))
            if (states.states.isEmpty()) Text("No saved states yet. Save one while the device runs; loading it later takes the device back to that moment.",
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(vertical = 16.dp))
            LazyColumn(Modifier.heightIn(max = 520.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(states.states, key = { it.id }) { state ->
                    StateRow(states, state, canLoad = states.job == null && host.restoring == null && host.shownStatus != "starting" && host.shownStatus != "stopping",
                        onLoad = { if (running) loading = state else { host.loadState(state); onDismiss() } },
                        onRename = { renaming = state }, onDelete = { deleting = state })
                }
            }
        }
    }
    loading?.let { state ->
        AlertDialog(onDismissRequest = { loading = null }, modifier = DialogTags,
            icon = { Icon(LtIcons.Save, null) },
            title = { Text("Load “${state.name}”?", style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center) },
            text = { Text("${device?.name ?: "The device"} goes back to that moment: what it has done since is replaced, as if it never happened. Save a state first to keep it.",
                style = MaterialTheme.typography.bodyMedium) },
            confirmButton = { Button(onClick = { loading = null; host.loadState(state); onDismiss() }, modifier = Modifier.testTag("confirm_load_state")) { Text("Load") } },
            dismissButton = { TextButton(onClick = { loading = null }) { Text("Cancel") } })
    }
    deleting?.let { state ->
        AlertDialog(onDismissRequest = { deleting = null }, modifier = DialogTags,
            icon = { Icon(Icons.Filled.Delete, null) },
            title = { Text("Delete “${state.name}”?", style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center) },
            text = { Text("The save state is removed from this phone. The device itself isn't changed.", style = MaterialTheme.typography.bodyMedium) },
            confirmButton = { Button(onClick = { deleting = null; host.deleteState(state) },
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error)) { Text("Delete") } },
            dismissButton = { TextButton(onClick = { deleting = null }) { Text("Cancel") } })
    }
    renaming?.let { state ->
        var name by remember(state.id) { mutableStateOf(state.name) }
        val fits = name.isNotBlank() && name.trim().toByteArray().size < 128
        AlertDialog(onDismissRequest = { renaming = null }, modifier = DialogTags,
            title = { Text("Rename save state", style = MaterialTheme.typography.titleMedium) },
            text = { OutlinedTextField(name, { name = it }, singleLine = true) },
            confirmButton = { Button(onClick = { renaming = null; host.renameState(state, name) }, enabled = fits) { Text("Save") } },
            dismissButton = { TextButton(onClick = { renaming = null }) { Text("Cancel") } })
    }
}

@Composable
private fun StateRow(states: SaveStatesController, state: SaveStateSummary, canLoad: Boolean, onLoad: () -> Unit,
                     onRename: () -> Unit, onDelete: () -> Unit) {
    // A card per state, as the desktop's panel: its screen at the device's shape, the whole name (two lines at most),
    // when and how big, and Load as the main action.
    Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(10.dp),
        verticalAlignment = Alignment.CenterVertically) {
        val height = 96.dp
        val width = if (state.width > 0 && state.height > 0) height * (state.width.toFloat() / state.height).coerceAtMost(1.5f) else height * 0.66f
        val bitmap = remember(state.thumbnail) { states.thumbnail(state) }
        Box(Modifier.size(width, height).clip(RoundedCornerShape(8.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest)) {
            if (bitmap != null) Image(bitmap.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(state.name, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Text("${DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(state.createdMs))} · ${size(state.bytes)}",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(onClick = onLoad, enabled = canLoad, contentPadding = PaddingValues(horizontal = 18.dp),
                    modifier = Modifier.height(36.dp).testTag("load_state")) { Text("Load") }
                Spacer(Modifier.width(4.dp))
                IconButton(onClick = onRename) { Icon(Icons.Filled.Edit, "Rename", Modifier.size(18.dp)) }
                IconButton(onClick = onDelete) { Icon(Icons.Filled.Delete, "Delete", Modifier.size(18.dp), tint = MaterialTheme.colorScheme.error) }
            }
        }
    }
}

private fun size(bytes: Long): String =
    if (bytes >= 1L shl 30) "%.1f GB".format(bytes / (1L shl 30).toDouble()) else "${bytes shr 20} MB"

/**
 * A save state loading into the device on screen (GuestHost.restoring): the state's picture, its saved screen, where the
 * guest's frames go, greyed out with the load's progress and percentage in the middle, until the state runs. No boot and
 * no stopped screen show.
 */
@Composable
internal fun RestoringOverlay(host: GuestHost, modifier: Modifier = Modifier) {
    val state = host.restoring ?: return
    val bitmap = remember(state.thumbnail) { host.saveStates.thumbnail(state) }
    Box(modifier.background(Color.Black).testTag("restoring_state")) {
        if (bitmap != null) Image(bitmap.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        // Greyed out, with the load's progress in the middle on a dark plate that keeps it readable over any picture.
        Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.6f)))
        // The furthest the load has got: between its steps (the old session ending, the new engine starting) the job
        // reports no fraction for a moment, and the bar holds rather than going back to sliding.
        var furthest by remember(state.id) { mutableStateOf<Float?>(null) }
        host.saveStates.job?.fraction?.let { f -> if (f > (furthest ?: -1f)) furthest = f }
        val fraction = furthest
        Column(Modifier.align(Alignment.Center).clip(RoundedCornerShape(14.dp)).background(Color.Black.copy(alpha = 0.65f))
            .padding(horizontal = 20.dp, vertical = 14.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            Text("Restoring…" + (fraction?.let { " ${(it * 100).toInt()}%" } ?: ""), color = Color.White,
                style = MaterialTheme.typography.labelLarge, modifier = Modifier.testTag("restoring_progress"))
            Spacer(Modifier.height(10.dp))
            val bar = Modifier.width(180.dp).height(6.dp).clip(RoundedCornerShape(3.dp))
            if (fraction != null) LinearProgressIndicator(progress = { fraction }, modifier = bar, trackColor = Color.White.copy(alpha = 0.2f), drawStopIndicator = {})
            else LinearProgressIndicator(modifier = bar, trackColor = Color.White.copy(alpha = 0.2f))
        }
    }
}
