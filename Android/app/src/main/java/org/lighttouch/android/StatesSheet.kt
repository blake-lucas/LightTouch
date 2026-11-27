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
            Text("Save states", style = MaterialTheme.typography.titleMedium)
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
            Spacer(Modifier.height(12.dp))
            HorizontalDivider()
            if (states.states.isEmpty()) Text("No save states yet. Save one while the device runs, and load it later to carry on from that moment.",
                style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(vertical = 16.dp))
            LazyColumn(Modifier.heightIn(max = 460.dp)) {
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
    Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        val height = 72.dp
        val width = if (state.width > 0 && state.height > 0) height * (state.width.toFloat() / state.height) else height * 0.66f
        val bitmap = remember(state.thumbnail) { states.thumbnail(state) }
        Box(Modifier.size(width, height).clip(RoundedCornerShape(6.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest)) {
            if (bitmap != null) Image(bitmap.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(state.name, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text("${DateFormat.getDateTimeInstance(DateFormat.MEDIUM, DateFormat.SHORT).format(Date(state.createdMs))} · ${size(state.bytes)}",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = onLoad, enabled = canLoad, modifier = Modifier.testTag("load_state")) { Text("Load") }
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
 * guest's frames go, with a thin bar of the load's progress, until the state runs. No boot and no stopped screen show.
 */
@Composable
internal fun RestoringOverlay(host: GuestHost, modifier: Modifier = Modifier) {
    val state = host.restoring ?: return
    val bitmap = remember(state.thumbnail) { host.saveStates.thumbnail(state) }
    Box(modifier.background(Color.Black).testTag("restoring_state")) {
        if (bitmap != null) Image(bitmap.asImageBitmap(), null, Modifier.fillMaxSize(), contentScale = ContentScale.Fit)
        val fraction = host.saveStates.job?.fraction
        val bar = Modifier.align(Alignment.BottomCenter).padding(bottom = 14.dp).fillMaxWidth(0.4f).height(3.dp).clip(RoundedCornerShape(2.dp))
        if (fraction != null) LinearProgressIndicator(progress = { fraction }, modifier = bar, color = Color.White, trackColor = Color.White.copy(alpha = 0.25f))
        else LinearProgressIndicator(modifier = bar, color = Color.White, trackColor = Color.White.copy(alpha = 0.25f))
    }
}
