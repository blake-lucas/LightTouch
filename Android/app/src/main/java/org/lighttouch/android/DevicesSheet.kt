package org.lighttouch.android

import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.KeyboardActions
import android.os.Build
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

/** Every device in the library: pick one, add another, rename or delete. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun DevicesSheet(host: GuestHost, onDismiss: () -> Unit) {
    var renaming by remember { mutableStateOf<DeviceSummary?>(null) }
    var deleting by remember { mutableStateOf<DeviceSummary?>(null) }
    ModalBottomSheet(onDismissRequest = onDismiss, containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), modifier = DialogTags.testTag("devices_sheet")) {
        SheetSystemBars()
        Column(Modifier.navigationBarsPadding().padding(bottom = 16.dp)) {
            Text("Devices", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp))
            if (host.active) Text("Stop ${host.selected?.name ?: "the running device"} to switch to or add another.",
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp))
            LazyColumn(Modifier.padding(top = 8.dp).weight(1f, fill = false)) {
                items(host.devices, key = { it.id }) { device ->
                    // A running device's record is marked in use, which is not a crash to recover from.
                    DeviceRow(device, switchable = !host.active && !host.busy, editable = !(host.active && device.selected),
                        needsRecovery = device.dirty && !(host.active && device.selected),
                        onSelect = { host.selectDevice(device.id); onDismiss() },
                        onDetails = { host.openDetails(device) }, onRename = { renaming = device }, onDelete = { deleting = device })
                }
            }
            Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp).clip(RoundedCornerShape(14.dp))
                .clickable(enabled = !host.busy && !host.active) { onDismiss(); host.addDevice() }.padding(12.dp).testTag("add_device").alpha(if (host.active) 0.38f else 1f),
                verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(44.dp).background(MaterialTheme.colorScheme.primaryContainer, RoundedCornerShape(12.dp)), contentAlignment = Alignment.Center) {
                    Icon(Icons.Filled.Add, null, tint = MaterialTheme.colorScheme.onPrimaryContainer)
                }
                Spacer(Modifier.width(16.dp))
                Column {
                    Text("Add device", style = MaterialTheme.typography.titleSmall)
                    Text("Prepared from an IPSW", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
    }
    renaming?.let { device -> RenameDialog(device, { renaming = null }) { host.renameDevice(device.id, it); renaming = null } }
    deleting?.let { device ->
        AlertDialog(onDismissRequest = { deleting = null }, modifier = DialogTags, icon = { Icon(Icons.Filled.Delete, null) },
            title = { Text("Delete ${device.name}?", style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center) },
            text = { Text("Its apps, settings and data are removed from this phone. This can't be undone.") },
            confirmButton = { Button(onClick = { host.deleteDevice(device.id); deleting = null },
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error), modifier = Modifier.testTag("confirm_delete")) { Text("Delete") } },
            dismissButton = { TextButton(onClick = { deleting = null }) { Text("Cancel") } })
    }
}

@Composable
private fun DeviceRow(device: DeviceSummary, switchable: Boolean, editable: Boolean, needsRecovery: Boolean, onSelect: () -> Unit, onDetails: () -> Unit, onRename: () -> Unit, onDelete: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp).clip(RoundedCornerShape(14.dp))
        .background(if (device.selected) MaterialTheme.colorScheme.surfaceContainerHighest else MaterialTheme.colorScheme.surfaceContainerLow)
        .clickable(enabled = switchable && !device.selected, onClick = onSelect).padding(start = 12.dp, top = 10.dp, bottom = 10.dp)
        .testTag("device_row"), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(44.dp).background(MaterialTheme.colorScheme.surfaceContainerHigh, RoundedCornerShape(12.dp)), contentAlignment = Alignment.Center) {
            Icon(LtIcons.Phone, null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Spacer(Modifier.width(16.dp))
        Column(Modifier.weight(1f)) {
            Text(device.name, style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(device.subtitle + if (needsRecovery) " · needs recovery" else "", style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (device.selected) Icon(Icons.Filled.Check, "Selected", tint = MaterialTheme.colorScheme.primary)
        Box {
            IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "Options for ${device.name}") }
            DropdownMenu(menu, { menu = false }) {
                DropdownMenuItem(text = { Text("Details") }, leadingIcon = { Icon(LtIcons.Info, null) }, onClick = { menu = false; onDetails() },
                    modifier = Modifier.testTag("device_details_item"))
                DropdownMenuItem(text = { Text("Rename") }, leadingIcon = { Icon(Icons.Filled.Edit, null) }, onClick = { menu = false; onRename() })
                DropdownMenuItem(text = { Text("Delete") }, leadingIcon = { Icon(Icons.Filled.Delete, null) }, enabled = editable,
                    onClick = { menu = false; onDelete() })
            }
        }
    }
    Spacer(Modifier.height(4.dp))
}

@Composable
internal fun RenameDialog(device: DeviceSummary, onDismiss: () -> Unit, onRename: (String) -> Unit) {
    // Opens with the whole name selected, so typing replaces it and a tap places the cursor.
    var name by remember { mutableStateOf(TextFieldValue(device.name, TextRange(0, device.name.length))) }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val save = { if (name.text.isNotBlank()) onRename(name.text.trim()) }
    AlertDialog(onDismissRequest = onDismiss, modifier = DialogTags, title = { Text("Rename device", style = MaterialTheme.typography.titleMedium) },
        text = { OutlinedTextField(name, { if (fitsDeviceName(it.text)) name = it }, singleLine = true, label = { Text("Name") },
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), keyboardActions = KeyboardActions(onDone = { save() }),
            modifier = Modifier.fillMaxWidth().focusRequester(focus).testTag("rename_field")) },
        confirmButton = { TextButton(onClick = save, enabled = name.text.isNotBlank(), modifier = Modifier.testTag("rename_save")) { Text("Save") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
}

/** The sheet's own window follows the system theme; match the app's instead, without the nav-bar scrim. */
@Composable
private fun SheetSystemBars() {
    val window = (LocalView.current.parent as? DialogWindowProvider)?.window ?: return
    val light = MaterialTheme.colorScheme.surface.luminance() > 0.5f
    SideEffect {
        if (Build.VERSION.SDK_INT >= 29) window.isNavigationBarContrastEnforced = false
        WindowCompat.getInsetsController(window, window.decorView).run { isAppearanceLightNavigationBars = light; isAppearanceLightStatusBars = light }
    }
}
