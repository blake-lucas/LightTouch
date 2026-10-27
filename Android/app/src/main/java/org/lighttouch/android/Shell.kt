package org.lighttouch.android

import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

enum class Screen { LOADING, SETUP, HOME, FULLSCREEN, SETTINGS, STORE, HITCH, DETAILS }

/** What the screens read from and ask of the activity, which owns the worker and its lifecycle. */
interface GuestHost {
    val status: String
    val sessionOwned: Boolean
    val displayAsleep: Boolean
    val busy: Boolean
    val devices: List<DeviceSummary>
    val selected: DeviceSummary?
    val error: String
    val networkSessionStatus: NetworkSessionStatus
    val networkChangeNotice: String
    val settings: AppSettings
    val perf: PerfSnapshot
    val profileRecording: Boolean
    val profileAutomatic: Boolean
    val profileBusy: Boolean
    val profileNotice: String
    val storeInstalling: Boolean
    /** The engine can save this guest's state (Save & Stop); false until supported on this host. */
    val canSaveState: Boolean
    fun dismissError()
    fun start()
    fun recover()
    fun stop()
    fun saveAndStop()
    fun setPaused(paused: Boolean)
    fun button(id: Int, down: Boolean)
    fun pulse(id: Int)
    fun screenshot()
    fun navigate(screen: Screen)
    fun openDevices()
    fun openDetails(device: DeviceSummary)
    fun openHitch()
    fun selectDevice(id: String)
    fun renameDevice(id: String, name: String)
    fun deleteDevice(id: String)
    fun addDevice()
    fun toggleProfile()
    fun pickGuestTools()
    fun openLicenses()
    fun openPreparation()
    fun applySettings()
}

internal val GuestHost.running get() = status == "running"
internal val GuestHost.paused get() = status == "paused"
internal val GuestHost.active get() = status in listOf("running", "paused", "starting", "stopping")

/** A labelled icon button for the control dock. */
@Composable
internal fun DockButton(icon: ImageVector, label: String, onClick: () -> Unit, modifier: Modifier = Modifier,
                        enabled: Boolean = true, tint: Color = MaterialTheme.colorScheme.onSurface, tag: String? = null) {
    Column(modifier.clip(RoundedCornerShape(14.dp)).clickable(enabled = enabled, onClick = onClick)
        .then(if (tag != null) Modifier.testTag(tag) else Modifier).semantics(mergeDescendants = true) { contentDescription = label }
        .padding(horizontal = 4.dp, vertical = 8.dp).alpha(if (enabled) 1f else 0.38f),
        horizontalAlignment = Alignment.CenterHorizontally) {
        Icon(icon, null, Modifier.size(24.dp), tint = tint)
        Spacer(Modifier.height(4.dp))
        Text(label, style = MaterialTheme.typography.labelSmall, color = tint, maxLines = 1, textAlign = TextAlign.Center)
    }
}

/** A dismissible error card; long messages expand on tap. */
@Composable
internal fun ErrorBanner(message: String, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    AnimatedVisibility(message.isNotEmpty(), modifier) {
        var expanded by remember(message) { mutableStateOf(false) }
        Surface(color = MaterialTheme.colorScheme.errorContainer, shape = RoundedCornerShape(14.dp), shadowElevation = 6.dp,
            modifier = Modifier.fillMaxWidth().testTag("session_error").clickable { expanded = !expanded }) {
            Row(Modifier.padding(start = 14.dp, top = 10.dp, bottom = 10.dp), verticalAlignment = Alignment.Top) {
                Icon(Icons.Filled.Warning, null, Modifier.size(18.dp).padding(top = 1.dp), tint = MaterialTheme.colorScheme.onErrorContainer)
                Spacer(Modifier.width(10.dp))
                Text(message, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onErrorContainer,
                    maxLines = if (expanded) Int.MAX_VALUE else 2, overflow = TextOverflow.Ellipsis)
                IconButton(onClick = onDismiss, modifier = Modifier.size(32.dp)) {
                    Icon(Icons.Filled.Close, "Dismiss", Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onErrorContainer)
                }
            }
        }
    }
}

/** A round translucent control drawn over the guest screen. */
@Composable
internal fun ScreenAction(icon: ImageVector, label: String, onClick: () -> Unit, modifier: Modifier = Modifier, tag: String? = null) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        Box(Modifier.size(64.dp).background(Color.White.copy(alpha = 0.14f), CircleShape).clip(CircleShape)
            .clickable(onClick = onClick).then(if (tag != null) Modifier.testTag(tag) else Modifier)
            .semantics { contentDescription = label }, contentAlignment = Alignment.Center) {
            Icon(icon, null, Modifier.size(32.dp), tint = Color.White)
        }
        Spacer(Modifier.height(10.dp))
        Text(label, style = MaterialTheme.typography.labelLarge, color = Color.White.copy(alpha = 0.85f), textAlign = TextAlign.Center)
    }
}

internal fun statusLabel(status: String) = when (status) {
    "running" -> "Running"; "starting" -> "Starting"; "paused" -> "Paused"; "stopping" -> "Shutting down"
    "failed" -> "Stopped unexpectedly"; else -> "Off"
}

/** Dialogs get their own window, so they repeat the activity's opt-in to expose test tags as resource ids. */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
internal val DialogTags = Modifier.semantics { testTagsAsResourceId = true }
