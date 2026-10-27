package org.lighttouch.android

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

@OptIn(ExperimentalMaterial3Api::class, androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun NetworkSettingsScreen(host: GuestHost, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val settings = host.settings
    val config = settings.network
    var showDate by rememberSaveable { mutableStateOf(false) }
    var showHttps by rememberSaveable { mutableStateOf(false) }
    var saveError by remember { mutableStateOf<String?>(null) }
    val update: (NetworkConfiguration) -> Unit = { next ->
        try { settings.network = next; host.applySettings(); saveError = null }
        catch (_: Exception) { saveError = "Couldn’t save network settings. Check available storage and try again." }
    }
    if (showDate) {
        val today = LocalDate.now()
        val picker = rememberDatePickerState(initialSelectedDateMillis = config.date.atStartOfDay(ZoneOffset.UTC).toInstant().toEpochMilli(),
            yearRange = 1996..today.year,
            selectableDates = object : SelectableDates {
                override fun isSelectableDate(utcTimeMillis: Long): Boolean = Instant.ofEpochMilli(utcTimeMillis).atZone(ZoneOffset.UTC).toLocalDate() <= today
            })
        DatePickerDialog(onDismissRequest = { showDate = false },
            confirmButton = { TextButton(enabled = picker.selectedDateMillis != null, onClick = {
                picker.selectedDateMillis?.let { selected ->
                    update(config.copy(archiveDate = Instant.ofEpochMilli(selected).atZone(ZoneOffset.UTC).toLocalDate().format(DateTimeFormatter.BASIC_ISO_DATE)))
                }; showDate = false
            }, modifier = Modifier.testTag("network_date_confirm")) { Text("Use date") } },
            dismissButton = { TextButton(onClick = { showDate = false }) { Text("Cancel") } }) {
            DatePicker(state = picker, title = { Text("Archive date", Modifier.padding(start = 24.dp, top = 16.dp)) })
        }
    }
    if (showHttps) AlertDialog(onDismissRequest = { showHttps = false },
        title = { Text("HTTPS compatibility") },
        text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Old versions of iOS can’t connect to many HTTPS websites today. LightTouch gives the emulated device its own proxy certificate, then connects to websites using modern TLS and your Android phone’s trusted certificates.")
            Text("The certificate is trusted only inside the emulated device. Android’s certificate settings stay as they are. Apps that pin certificates may still refuse the connection.")
            Text("Setup runs once the guest tools are available, at startup or when you turn networking on. If setup fails, update guest tools and restart the device.")
        } }, confirmButton = { TextButton(onClick = { showHttps = false }) { Text("Got it") } })
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }, color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack, modifier = Modifier.testTag("network_back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back to settings") }
                Text("Network", style = MaterialTheme.typography.titleLarge)
            }
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).testTag("network_settings")) {
                SettingsGroup("Connection") {
                    SwitchRow(LtIcons.Network, "Enable networking", "Let the emulated device connect through your phone’s internet connection", config.enabled, "network_enabled") { update(config.copy(enabled = it)) }
                }
                Text("Changes apply right away, even while the device is running: reload the page to use them. The first time networking is on in a session, the device takes a few seconds to set up.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp).testTag("network_live_note"))
                saveError?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp)) }
                SettingsGroup("Browse with") {
                    RouteRow(NetworkRoute.DIRECT, "Connect to websites as they are today", config.route) { update(config.copy(route = it)) }
                    RouteRow(NetworkRoute.ARCHIVE, "Visit pages preserved by the Internet Archive", config.route) { update(config.copy(route = it)) }
                    if (config.route == NetworkRoute.ARCHIVE) {
                        HorizontalDivider(Modifier.padding(horizontal = 16.dp), color = MaterialTheme.colorScheme.outlineVariant)
                        ActionRow(LtIcons.History, "Archive date", config.date.format(DateTimeFormatter.ofPattern("d MMMM yyyy")), tag = "network_archive_date") { showDate = true }
                        Text("Uses the closest available capture. Some pages, images and online services weren’t archived. Forms and sign-ins need Direct internet.",
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 54.dp, end = 16.dp, bottom = 16.dp))
                    }
                }
                SettingsGroup("Compatibility") {
                    ActionRow(LtIcons.Info, "HTTPS compatibility", "Connect old iOS to modern HTTPS websites", tag = "network_https_info") { showHttps = true }
                    if (config.route == NetworkRoute.DIRECT) SwitchRow(LtIcons.Network, "Allow local network", "Reach other devices on your Wi-Fi network. Off by default", config.localNetwork, "network_local") { update(config.copy(localNetwork = it)) }
                }
                if (host.active) SettingsGroup("Current session") {
                    NetworkSessionInfo(host.networkSessionStatus, host.networkChangeNotice)
                }
                Spacer(Modifier.height(24.dp))
            }
        }
    }
}

@Composable
internal fun NetworkSessionInfo(status: NetworkSessionStatus, changeNotice: String) {
    Column(Modifier.fillMaxWidth().padding(16.dp).testTag("network_status"), verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.Top) {
            Icon(LtIcons.Info, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            Text("Connection information for this session", style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        if (!status.available) {
            Text("Waiting for network status…", style = MaterialTheme.typography.bodyMedium)
        } else {
            SessionInfoLine("Connection", when (status.mode) {
                1 -> NetworkRoute.DIRECT.label
                2 -> NetworkRoute.ARCHIVE.label
                else -> "Networking is off"
            }, "network_session_connection")
            if (status.mode == 2) {
                val date = runCatching { LocalDate.parse(status.archiveDate, DateTimeFormatter.BASIC_ISO_DATE)
                    .format(DateTimeFormatter.ofPattern("d MMMM yyyy")) }.getOrDefault(status.archiveDate)
                SessionInfoLine("Archive date", date, "network_session_date")
            }
            if (status.mode != 0) {
                SessionInfoLine(if (status.setupError) "Guest setup needs attention" else "Guest setup", if (status.setup == "Ready") "Ready — certificate and proxy settings installed" else status.setup,
                    "network_session_setup", status.setupError)
                SessionInfoLine("Web activity", status.activity, "network_session_activity")
                if (status.connections > 0) SessionInfoLine("Session totals",
                    "Connections received: ${status.connections}\nFinished: ${status.completed} · Errors: ${status.failed}", "network_session_totals")
                if (status.lastResult.isNotEmpty()) SessionInfoLine(if (status.lastResultError) "Last result (error)" else "Last result", status.lastResult, "network_session_result", status.lastResultError)
                if (status.setup == "Ready" && status.connections == 0L) {
                    Text("Setup is complete. Open a page in Safari to check the connection. Being ready doesn’t confirm that a website can load.",
                        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (changeNotice.isNotEmpty()) SessionInfoLine("Settings update", changeNotice, "network_change_notice")
    }
}

@Composable
private fun SessionInfoLine(label: String, value: String, tag: String, error: Boolean = false) {
    Column(Modifier.fillMaxWidth().testTag(tag), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value, style = MaterialTheme.typography.bodyMedium,
            color = if (error) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
    }
}

@Composable
private fun RouteRow(route: NetworkRoute, subtitle: String, selected: NetworkRoute, onSelect: (NetworkRoute) -> Unit) {
    Row(Modifier.fillMaxWidth().selectable(selected == route, role = Role.RadioButton, onClick = { onSelect(route) })
        .testTag(if (route == NetworkRoute.DIRECT) "network_direct" else "network_wayback")
        .padding(horizontal = 16.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(if (route == NetworkRoute.DIRECT) LtIcons.Network else LtIcons.History, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.width(16.dp))
        Column(Modifier.weight(1f)) {
            Text(route.label, style = MaterialTheme.typography.bodyLarge)
            Text(subtitle, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Spacer(Modifier.width(8.dp))
        RadioButton(selected == route, onClick = null)
    }
}
