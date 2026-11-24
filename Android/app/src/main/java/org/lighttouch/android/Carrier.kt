package org.lighttouch.android

import android.os.Bundle
import android.os.Handler
import android.os.RemoteException
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import org.json.JSONObject

// Carrier: the fake cellular network of a device with a modem (iPhone 3GS, iPhone 4), like Light Touch for Mac's
// Carrier panel. The settings are the device's, saved with its record by the shared core, which applies them at every
// boot and to a running modem (EngineService.setCarrier). Calls and texts go straight to the running modem.

/** A device's carrier settings (Portable lt_carrier). */
data class CarrierSettings(
    val carrier: String, val mccMnc: String, val registered: Boolean, val simPresent: Boolean, val bars: Int,
    val latitude: Double, val longitude: Double, val dockButton: Boolean, val lastCaller: String, val lastSender: String,
) {
    val mcc get() = mccMnc.take(3)
    val mnc get() = mccMnc.drop(3)
    val summary get() = when {
        !simPresent -> "No SIM"
        !registered -> "No Service"
        else -> "$carrier · $bars bar${if (bars == 1) "" else "s"}"
    }

    fun toBundle() = Bundle().apply {
        putString("carrier", carrier); putString("mccMnc", mccMnc); putBoolean("registered", registered)
        putBoolean("simPresent", simPresent); putInt("bars", bars); putDouble("latitude", latitude); putDouble("longitude", longitude)
        putBoolean("dockButton", dockButton); putString("lastCaller", lastCaller); putString("lastSender", lastSender)
    }

    companion object {
        fun of(b: Bundle) = CarrierSettings(b.getString("carrier") ?: "", b.getString("mccMnc") ?: "", b.getBoolean("registered"),
            b.getBoolean("simPresent"), b.getInt("bars"), b.getDouble("latitude"), b.getDouble("longitude"), b.getBoolean("dockButton"),
            b.getString("lastCaller") ?: "", b.getString("lastSender") ?: "")
        val APPLE_PARK = 37.3349 to -122.0090
    }
}

/** The modem's state as the core reports it (lt_runtime_modem_status). */
data class ModemStatus(
    val carrier: String, val callState: String, val lastDialed: String, val lastMoSms: String, val moSmsCount: Long,
    val gps: Boolean, val error: String?, val pending: Boolean,
) {
    /** The other end of the call the device is placing can pick up. */
    val canAnswer get() = callState == "dialing" || callState == "alerting"
    val inCall get() = callState.isNotEmpty() && callState != "idle"
    val callLabel get() = when (callState) {
        "dialing" -> "Calling out"; "alerting" -> "Calling out (ringing)"; "incoming" -> "Ringing"
        "active" -> "Connected"; "held" -> "On hold"; else -> "No call"
    }
    /** The last text the device sent, as (number, text); "|" (none yet) has no number. */
    val lastSent get() = lastMoSms.indexOf('|').takeIf { it > 0 }?.let { lastMoSms.take(it) to lastMoSms.drop(it + 1) }

    companion object {
        fun parse(json: String): ModemStatus? = runCatching {
            val o = JSONObject(json)
            ModemStatus(o.optString("carrier"), o.optString("call-state", "idle"), o.optString("last-dialed"),
                o.optString("last-mo-sms"), o.optLong("mo-sms-count"), o.optBoolean("gps"),
                if (o.isNull("error")) null else o.optString("error").ifEmpty { null }, o.optBoolean("pending"))
        }.getOrNull()
    }
}

/** The modem's refusal in words for people: the engine's own sentences name AT-level conditions. */
internal fun refusalText(error: String) = if (error.startsWith("the modem is not in a state"))
    "The device isn't on the network yet: iOS may still be starting, or Registered or SIM card is off" else error

/** The modem's rules (Portable Carrier.hpp), for hints while typing; the core checks again when saving or sending. */
internal object CarrierRules {
    fun numberOk(s: String): Boolean { val d = s.removePrefix("+"); return d.length in 1..20 && d.all { it in '0'..'9' } }
    fun carrierOk(s: String): Boolean { val bytes = s.toByteArray(); return bytes.size in 1..32 && s.none { it < ' ' || it == '\u007f' || it == '"' } }
    fun plmnOk(mcc: String, mnc: String) = mcc.length == 3 && mnc.length in 2..3 && (mcc + mnc).all { it in '0'..'9' }

    private val single = ("@£\$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑ§¿" +
        " abcdefghijklmnopqrstuvwxyzäöüñà").toSet()
    private val escaped = "^{}\\[]|€".toSet()

    /** (used, limit): GSM 7-bit septets of 160 when every character is in that alphabet, else UTF-16 units of 70;
     *  limit 0 for characters beyond U+FFFF, which the modem can't send. */
    fun smsLength(text: String): Pair<Int, Int> {
        if (text.any { it.isSurrogate() }) return text.length to 0
        if (text.all { it in single || it in escaped }) return text.length + text.count { it in escaped } to 160
        return text.length to 70
    }
    fun smsOk(text: String) = text.isNotEmpty() && smsLength(text).let { (used, limit) -> used <= limit }
}

/** The carrier controls' state for the current device, held by the activity. Engine calls run on a background thread. */
class CarrierController(private val engine: () -> IEngine?, private val handler: Handler, private val onError: (String) -> Unit) {
    var deviceId by mutableStateOf(""); private set
    var settings by mutableStateOf<CarrierSettings?>(null); private set
    var status by mutableStateOf<ModemStatus?>(null); private set
    /** What the modem or the core refused last, until the next action. */
    var message by mutableStateOf(""); private set
    /** Texts the device sent this session (number, text), oldest first. */
    val sent = mutableStateListOf<Pair<String, String>>()

    private fun background(name: String, work: (IEngine) -> String) {
        val worker = engine() ?: return
        Thread({
            val result = try { work(worker) } catch (e: RemoteException) { e.message ?: "Worker unavailable" }
            handler.post { if (result.isNotEmpty()) { message = result; onError("Carrier: $result") } }
        }, name).start()
    }

    /** Follow the current device: load its settings when it changes (devices without a modem have none). */
    fun follow(device: DeviceSummary?) {
        val id = device?.takeIf { it.cellular }?.id ?: ""
        if (id == deviceId) return
        deviceId = id; settings = null; status = null; message = ""; sent.clear()
        if (id.isEmpty()) return
        val worker = engine() ?: return
        Thread({
            val b = try { worker.carrier(id) } catch (_: RemoteException) { null }
            handler.post { if (deviceId == id && b != null && b.getString("error").isNullOrEmpty()) settings = CarrierSettings.of(b) }
        }, "carrier-load").start()
    }

    /** Save (a running device takes what changed); shown at once, put back if the core refuses. */
    fun save(next: CarrierSettings) {
        val id = deviceId.takeIf { it.isNotEmpty() } ?: return
        val previous = settings
        settings = next; message = ""
        val worker = engine() ?: return
        Thread({
            val result = try { worker.setCarrier(id, next.toBundle()) } catch (e: RemoteException) { e.message ?: "Worker unavailable" }
            handler.post { if (result.isNotEmpty() && deviceId == id) { message = result; onError("Carrier: $result"); if (settings == next) settings = previous } }
        }, "carrier-save").start()
    }

    private fun modem(action: String, value: String) { message = ""; background("carrier-modem") { it.modem(action, value) } }

    fun ring(number: String) {
        val n = number.trim()
        settings?.let { if (it.lastCaller != n) save(it.copy(lastCaller = n)) }
        modem("incoming-call", n)
    }
    fun text(number: String, body: String) {
        val n = number.trim()
        settings?.let { if (it.lastSender != n) save(it.copy(lastSender = n)) }
        modem("incoming-sms", "$n|$body")
    }
    fun answer() = modem("remote-answer", "1")
    fun hangUp() = modem("remote-hangup", "1")

    /** One poll, on the activity's poll thread: the modem's state while the device runs. */
    fun poll(worker: IEngine?, running: Boolean) {
        if (deviceId.isEmpty()) return
        val json = if (running) try { worker?.modemStatus() ?: "" } catch (_: RemoteException) { "" } else ""
        val next = ModemStatus.parse(json)
        handler.post {
            val old = status
            if (next != null && old != null && next.moSmsCount > old.moSmsCount) next.lastSent?.let { sent.add(it); if (sent.size > 20) sent.removeAt(0) }
            if (next != null && !next.pending && next.error != null && (old?.error != next.error || old.pending)) {
                message = refusalText(next.error); onError("Carrier: $message")
            }
            status = next
        }
    }
}

private enum class CarrierDialog { CALL, TEXT }

/** Call… and Send text…, asked from the dock's menu or full screen's. */
@Composable
private fun CarrierDialogs(carrier: CarrierController, dialog: CarrierDialog?, onDismiss: () -> Unit) {
    val saved = carrier.settings ?: return
    when (dialog) {
        CarrierDialog.CALL -> {
            var number by rememberSaveable { mutableStateOf(saved.lastCaller) }
            val valid = CarrierRules.numberOk(number.trim())
            AlertDialog(onDismissRequest = onDismiss, modifier = DialogTags,
                icon = { Icon(LtIcons.Call, null) },
                title = { Text("Call the device", style = MaterialTheme.typography.titleMedium) },
                text = {
                    Column {
                        Text("It rings from this number; answer it on the device.", style = MaterialTheme.typography.bodyMedium)
                        Spacer(Modifier.height(12.dp))
                        OutlinedTextField(number, { number = it }, label = { Text("Caller") }, singleLine = true,
                            isError = number.isNotBlank() && !valid,
                            supportingText = { if (number.isNotBlank() && !valid) Text("1 to 20 digits, optionally after a +") },
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone), modifier = Modifier.testTag("carrier_caller"))
                    }
                },
                confirmButton = { Button(onClick = { onDismiss(); carrier.ring(number) }, enabled = valid, modifier = Modifier.testTag("carrier_call")) { Text("Call") } },
                dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
        }
        CarrierDialog.TEXT -> {
            var number by rememberSaveable { mutableStateOf(saved.lastSender) }
            var body by rememberSaveable { mutableStateOf("") }
            val numberOk = CarrierRules.numberOk(number.trim())
            val (used, limit) = CarrierRules.smsLength(body)
            AlertDialog(onDismissRequest = onDismiss, modifier = DialogTags,
                icon = { Icon(LtIcons.Sms, null) },
                title = { Text("Send a text to the device", style = MaterialTheme.typography.titleMedium) },
                text = {
                    Column {
                        OutlinedTextField(number, { number = it }, label = { Text("From") }, singleLine = true,
                            isError = number.isNotBlank() && !numberOk,
                            supportingText = { if (number.isNotBlank() && !numberOk) Text("1 to 20 digits, optionally after a +") },
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone), modifier = Modifier.testTag("carrier_sender"))
                        OutlinedTextField(body, { body = it }, label = { Text("Message") }, minLines = 3, maxLines = 6,
                            isError = used > limit,
                            supportingText = { Text(if (limit == 0) "Emoji can't be sent in a text this old" else "$used / $limit") },
                            modifier = Modifier.fillMaxWidth().testTag("carrier_message"))
                    }
                },
                confirmButton = { Button(onClick = { onDismiss(); carrier.text(number, body) }, enabled = numberOk && body.isNotEmpty() && used <= limit,
                    modifier = Modifier.testTag("carrier_send")) { Text("Send") } },
                dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } })
        }
        null -> {}
    }
}

/** The Carrier menu's items: the call's state and controls, Call…, Send text… and the settings. */
@Composable
private fun CarrierMenuItems(host: GuestHost, close: () -> Unit, ask: (CarrierDialog) -> Unit) {
    val carrier = host.carrier
    val status = carrier.status
    val onNetwork = carrier.settings?.let { it.registered && it.simPresent } ?: false
    Text(status?.callLabel ?: "No call", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp).testTag("carrier_call_state"))
    if (status?.canAnswer == true) DropdownMenuItem(text = { Text("Answer") }, leadingIcon = { Icon(LtIcons.Call, null) },
        onClick = { close(); carrier.answer() }, modifier = Modifier.testTag("carrier_answer"))
    if (status?.inCall == true) DropdownMenuItem(text = { Text("Hang up", color = LtColors.Danger) }, leadingIcon = { Icon(LtIcons.CallEnd, null, tint = LtColors.Danger) },
        onClick = { close(); carrier.hangUp() }, modifier = Modifier.testTag("carrier_hang_up"))
    DropdownMenuItem(text = { Text("Call…") }, leadingIcon = { Icon(LtIcons.Call, null) }, enabled = onNetwork && status?.inCall != true,
        onClick = { close(); ask(CarrierDialog.CALL) }, modifier = Modifier.testTag("carrier_menu_call"))
    DropdownMenuItem(text = { Text("Send text…") }, leadingIcon = { Icon(LtIcons.Sms, null) }, enabled = onNetwork,
        onClick = { close(); ask(CarrierDialog.TEXT) }, modifier = Modifier.testTag("carrier_menu_text"))
    HorizontalDivider()
    DropdownMenuItem(text = { Text("Carrier settings…") }, leadingIcon = { Icon(LtIcons.Signal, null) },
        onClick = { close(); host.navigate(Screen.CARRIER) }, modifier = Modifier.testTag("carrier_menu_settings"))
}

/** Whether the session shows the Carrier button: the device runs, has a modem, and its setting is on. */
internal val GuestHost.carrierButton get() = active && selected?.cellular == true && carrier.settings?.dockButton == true

/** The dock's Carrier button and its menu. */
@Composable
internal fun CarrierDockButton(host: GuestHost, modifier: Modifier = Modifier) {
    var menu by remember { mutableStateOf(false) }
    var dialog by remember { mutableStateOf<CarrierDialog?>(null) }
    val inCall = host.carrier.status?.inCall == true
    Box(modifier) {
        DockButton(LtIcons.Signal, "Carrier", { menu = true }, Modifier.fillMaxWidth(), enabled = host.status in listOf("running", "paused"),
            tint = if (inCall) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface, tag = "carrier_button")
        DropdownMenu(menu, { menu = false }) { CarrierMenuItems(host, { menu = false }) { dialog = it } }
    }
    CarrierDialogs(host.carrier, dialog) { dialog = null }
}

/** Full screen's Carrier key: the same menu. */
@Composable
internal fun CarrierKey(host: GuestHost, key: @Composable (onClick: () -> Unit) -> Unit) {
    var menu by remember { mutableStateOf(false) }
    var dialog by remember { mutableStateOf<CarrierDialog?>(null) }
    Box {
        key { menu = true }
        DropdownMenu(menu, { menu = false }) { CarrierMenuItems(host, { menu = false }) { dialog = it } }
    }
    CarrierDialogs(host.carrier, dialog) { dialog = null }
}

/** Settings → Carrier: the device's network, its location (the 3GS's and iPhone 5's GPS), the dock button, and calls and texts. */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun CarrierScreen(host: GuestHost, onBack: () -> Unit) {
    BackHandler(onBack = onBack)
    val carrier = host.carrier
    val device = host.selected
    val saved = carrier.settings
    Surface(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }, color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack, modifier = Modifier.testTag("carrier_back")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                Column {
                    Text("Carrier", style = MaterialTheme.typography.titleLarge)
                    device?.let { Text(it.name, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
                }
            }
            if (saved == null || device?.cellular != true) {
                Text(if (device?.cellular == false) "This device has no cellular modem." else "Loading…", Modifier.padding(24.dp))
                return@Column
            }
            Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).testTag("carrier_settings")) {
                NetworkGroup(saved, carrier::save)
                Text("Saved with this device and used every time it starts. Changes reach a running device at once; iOS can take a few seconds to update its status bar.",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp))
                if (device.board == "n88ap" || device.board == "n42ap" || carrier.status?.gps == true) LocationGroup(saved, carrier::save)
                SettingsGroup("Session dock") {
                    SwitchRow(LtIcons.Signal, "Show Carrier button", "A dock button for calling the device, sending it texts and these settings",
                        saved.dockButton, "carrier_dock_toggle") { carrier.save(saved.copy(dockButton = it)) }
                }
                // The calls and texts themselves are the dock button's (one control per action): this shows how they stand.
                SettingsGroup("This session") {
                    if (host.status !in listOf("running", "paused")) Text("Start the device to see its calls and texts here.",
                        style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(16.dp))
                    else {
                        val status = carrier.status
                        Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                            Icon(LtIcons.Call, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                            Spacer(Modifier.width(16.dp))
                            Column(Modifier.weight(1f)) {
                                Text(status?.let { if (it.canAnswer && it.lastDialed.isNotEmpty()) "${it.callLabel}: ${it.lastDialed}" else it.callLabel } ?: "No call",
                                    style = MaterialTheme.typography.bodyLarge, modifier = Modifier.testTag("carrier_page_call_state"))
                                Text(if (saved.dockButton) "The dock's Carrier button calls the device, sends it texts, answers and hangs up"
                                    else "Turn on Show Carrier button to call and text the device from the dock",
                                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                        Column(Modifier.padding(start = 54.dp, end = 16.dp, bottom = 12.dp)) {
                            if (carrier.sent.isEmpty()) Text("Texts the device sends appear here.", style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant)
                            else {
                                Text("Texts the device sent", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                for ((number, text) in carrier.sent.takeLast(5).reversed()) {
                                    Text("To $number", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 6.dp))
                                    Text(text, style = MaterialTheme.typography.bodyMedium)
                                }
                            }
                        }
                    }
                }
                if (carrier.message.isNotEmpty()) Text(carrier.message, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(16.dp).testTag("carrier_message_text"))
                Spacer(Modifier.height(24.dp))
            }
        }
    }
}

@Composable
private fun NetworkGroup(saved: CarrierSettings, save: (CarrierSettings) -> Unit) {
    var name by remember(saved.carrier) { mutableStateOf(saved.carrier) }
    var mcc by remember(saved.mccMnc) { mutableStateOf(saved.mcc) }
    var mnc by remember(saved.mccMnc) { mutableStateOf(saved.mnc) }
    SettingsGroup("Network") {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            OutlinedTextField(name, { name = it }, label = { Text("Carrier name") }, singleLine = true,
                isError = !CarrierRules.carrierOk(name), supportingText = { Text("Shown in the status bar. 1 to 32 characters, no quotes") },
                modifier = Modifier.fillMaxWidth().testTag("carrier_name"))
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(mcc, { mcc = it.filter(Char::isDigit).take(3) }, label = { Text("MCC") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), modifier = Modifier.width(96.dp).testTag("carrier_mcc"))
                Spacer(Modifier.width(12.dp))
                OutlinedTextField(mnc, { mnc = it.filter(Char::isDigit).take(3) }, label = { Text("MNC") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), modifier = Modifier.width(96.dp).testTag("carrier_mnc"))
                Spacer(Modifier.weight(1f))
                val edited = name != saved.carrier || mcc + mnc != saved.mccMnc
                val valid = CarrierRules.carrierOk(name) && CarrierRules.plmnOk(mcc, mnc)
                if (edited) Button({ save(saved.copy(carrier = name, mccMnc = mcc + mnc)) }, enabled = valid,
                    modifier = Modifier.testTag("carrier_apply")) { Text("Apply") }
            }
            Text("001 01 is the test network", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp, start = 4.dp))
        }
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(LtIcons.Signal, null, Modifier.size(22.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.width(16.dp))
                Text("Signal", style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
                Text("${saved.bars} bar${if (saved.bars == 1) "" else "s"}", style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Slider(saved.bars.toFloat(), { if (it.toInt() != saved.bars) save(saved.copy(bars = it.toInt())) }, valueRange = 0f..5f, steps = 4,
                modifier = Modifier.padding(start = 38.dp).testTag("carrier_bars"))
        }
        SwitchRow(LtIcons.Antenna, "Registered", "On the network. Off, iOS shows No Service", saved.registered, "carrier_registered") { save(saved.copy(registered = it)) }
        SwitchRow(LtIcons.SimCard, "SIM card", "Off, iOS shows No SIM", saved.simPresent, "carrier_sim") { save(saved.copy(simPresent = it)) }
    }
}

@Composable
private fun LocationGroup(saved: CarrierSettings, save: (CarrierSettings) -> Unit) {
    var latitude by remember(saved.latitude) { mutableStateOf("%.6f".format(java.util.Locale.ROOT, saved.latitude)) }
    var longitude by remember(saved.longitude) { mutableStateOf("%.6f".format(java.util.Locale.ROOT, saved.longitude)) }
    val lat = latitude.trim().toDoubleOrNull()
    val lon = longitude.trim().toDoubleOrNull()
    val valid = lat != null && lon != null && kotlin.math.abs(lat) <= 90 && kotlin.math.abs(lon) <= 180
    SettingsGroup("Location") {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
            Text("Where the iPhone's GPS says it is, in degrees", style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(latitude, { latitude = it }, label = { Text("Latitude") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), modifier = Modifier.weight(1f).testTag("carrier_latitude"))
                Spacer(Modifier.width(8.dp))
                OutlinedTextField(longitude, { longitude = it }, label = { Text("Longitude") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), modifier = Modifier.weight(1f).testTag("carrier_longitude"))
            }
            Row(Modifier.padding(top = 8.dp)) {
                Button({ save(saved.copy(latitude = lat!!, longitude = lon!!)) }, enabled = valid) { Text("Set") }
                Spacer(Modifier.width(8.dp))
                TextButton({ save(saved.copy(latitude = CarrierSettings.APPLE_PARK.first, longitude = CarrierSettings.APPLE_PARK.second)) }) { Text("Apple Park") }
            }
        }
    }
}
