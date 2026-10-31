package org.lighttouch.android

import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp

/** What the catalog said about one installed app. A lookup that failed is not kept as an answer: it is asked again. */
internal sealed class Lookup {
    class Found(val known: LegacyStore.Known) : Lookup()
    object Missing : Lookup()
}

/**
 * The apps the running guest has, and what the Legacy Store knows of each (its page, its icon, a newer version
 * that runs here). The list comes from the guest over the USB bridge; the catalog is asked once per app, one
 * request at a time and cached, and stops at the first failure rather than retrying every app.
 * Work runs on worker threads and posts back, like [StoreModel].
 */
internal class InstalledList(private val apps: GuestApps, private val muxSocket: () -> String?, private val target: () -> StoreTarget?,
                             private val confirmed: () -> Boolean, private val changed: (List<InstalledApp>) -> Unit) {
    private val handler = Handler(Looper.getMainLooper())
    var list by mutableStateOf<List<InstalledApp>?>(null)
    var loading by mutableStateOf(false)
    var error by mutableStateOf<String?>(null)
    var lookups by mutableStateOf<Map<String, Lookup>>(emptyMap())
    var lookupError by mutableStateOf<String?>(null)
    /** An installed app the catalog doesn't know (or hasn't answered for yet), shown on a page of its own. */
    var plain by mutableStateOf<InstalledApp?>(null)
    private var lookupGeneration = 0
    private var refreshGeneration = 0
    private var lastRefresh = 0L

    private fun background(name: String, work: () -> Unit) = Thread(work, name).start()
    private fun post(work: () -> Unit) = handler.post(work)

    /** Read the guest's list again; a recent one stands (the install queue brings a fresh one after each install). */
    fun refresh(force: Boolean = false) {
        val socket = muxSocket() ?: return
        if (!apps.available || loading) return
        if (!force && list != null && SystemClock.elapsedRealtime() - lastRefresh < 10_000) { lookUp(); return }
        val generation = ++refreshGeneration
        loading = true; error = null
        background("store-installed") {
            val outcome = runCatching { apps.installedApps(socket) }
            post {
                if (generation != refreshGeneration) return@post
                loading = false
                outcome.onSuccess { if (muxSocket() == socket) set(it) }
                    .onFailure { if (muxSocket() == socket) error = "Couldn't read the apps on this device: ${it.message}" }
            }
        }
    }

    /** The guest's current list, from any source. */
    fun set(next: List<InstalledApp>) {
        lastRefresh = SystemClock.elapsedRealtime()
        list = next; error = null
        changed(next)
        lookUp()
    }

    /** Ask the catalog about the apps it hasn't answered for; needs the guest's own device type, which decides what runs. */
    fun lookUp() {
        val t = target() ?: return
        val pending = list?.map { it.bundleId }?.filter { it !in lookups } ?: return
        if (!confirmed() || pending.isEmpty()) return
        val generation = ++lookupGeneration
        lookupError = null
        background("store-lookup") {
            for (id in pending) {
                val outcome = runCatching { LegacyStore.lookup(id, t) }
                post {
                    if (generation != lookupGeneration) return@post
                    outcome.onSuccess { found -> lookups = lookups + (id to (found?.let { Lookup.Found(it) } ?: Lookup.Missing)) }
                        .onFailure { lookupError = it.message ?: "Couldn't reach Legacy Store" }
                }
                if (outcome.isFailure || generation != lookupGeneration) break
            }
        }
    }

    /** The newest version that runs here, if it is newer than what [app] has. */
    fun update(app: InstalledApp): String? = (lookups[app.bundleId] as? Lookup.Found)?.let { StoreVersions.updateTo(app.version, it.known.newest) }

    /** What the catalog knew changes with the device (what runs on it) and its apps; the list itself only with the guest. */
    fun forgetLookups() { lookupGeneration++; lookups = emptyMap(); lookupError = null }

    /** Another guest (or none) is behind the socket: its list is gone. */
    fun guestChanged() { refreshGeneration++; loading = false; list = null; error = null; plain = null }

    fun reset() { guestChanged(); forgetLookups() }
}

@Composable
internal fun ColumnScope.InstalledTab(model: StoreModel, running: Boolean) {
    val installed = model.installedList
    LaunchedEffect(running, model.targetConfirmed) { if (running) installed.refresh() }
    val list = installed.list
    when {
        !running -> InstalledMessage("Start the device to see its apps", "The apps on a device are read from it while it runs.")
        !model.servicesAvailable -> InstalledMessage("Not available in this build", "Reading a device's apps is not included in this build.")
        installed.error != null && list == null -> InstalledMessage("Couldn't read this device's apps", installed.error ?: "", retry = { installed.refresh(true) })
        list == null -> LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp))
        list.isEmpty() -> InstalledMessage("No apps added yet", "Apps you install from the Legacy Store or from an IPA file show up here.")
        else -> {
            installed.lookupError?.let { message ->
                Row(Modifier.padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text("Couldn't check for newer versions. $message", Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                    TextButton(onClick = { installed.lookUp() }) { Text("Retry") }
                }
            }
            LazyColumn(Modifier.weight(1f).testTag("store_installed"), contentPadding = PaddingValues(horizontal = 8.dp, vertical = 4.dp)) {
                items(list, key = { it.bundleId }) { app -> InstalledRow(app, model) }
            }
        }
    }
}

@Composable
private fun ColumnScope.InstalledMessage(title: String, body: String, retry: (() -> Unit)? = null) {
    Column(Modifier.weight(1f).fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
        Icon(LtIcons.Bag, null, Modifier.size(40.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.height(12.dp))
        Text(title, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
        Text(body, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
        retry?.let { TextButton(onClick = it) { Text("Try again") } }
    }
}

@Composable
private fun InstalledRow(app: InstalledApp, model: StoreModel) {
    val installed = model.installedList
    val found = (installed.lookups[app.bundleId] as? Lookup.Found)?.known
    val update = installed.update(app)
    val queued = model.queuedBundle(app.bundleId)?.takeIf { it.active }
    Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).clickable { model.openInstalled(app) }.padding(horizontal = 8.dp, vertical = 8.dp).testTag("store_installed_row"),
        verticalAlignment = Alignment.CenterVertically) {
        AppIcon(found?.app?.text("icon_url"), 52, app.title)
        Spacer(Modifier.width(14.dp))
        Column(Modifier.weight(1f)) {
            Text(app.title, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleSmall)
            Text(app.version?.let { "Version $it" } ?: app.bundleId, maxLines = 1, overflow = TextOverflow.Ellipsis,
                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            when {
                queued != null -> Text(if (queued.state == QueueState.PENDING) "In the queue" else queued.phase, style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary, maxLines = 1)
                update != null -> Text("Version $update is available", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary, maxLines = 1)
            }
        }
    }
}

/** An installed app the Legacy Store has no page for: what the device itself says about it. */
@Composable
internal fun ColumnScope.InstalledPlainPage(model: StoreModel, app: InstalledApp) {
    val installed = model.installedList
    val lookup = installed.lookups[app.bundleId]
    // The answer can arrive while this page is open.
    LaunchedEffect(lookup) { (lookup as? Lookup.Found)?.let { installed.plain = null; model.open(it.known.app) } }
    Column(Modifier.weight(1f).padding(horizontal = 16.dp).testTag("store_installed_page")) {
        Row(Modifier.padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            AppIcon(null, 72, app.title)
            Spacer(Modifier.width(16.dp))
            Column {
                Text(app.title, style = MaterialTheme.typography.titleLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Text(app.version?.let { "Version $it" } ?: "", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        Spacer(Modifier.height(8.dp))
        Column(Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainer).padding(16.dp)) {
            Text("Identifier", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(app.bundleId, style = MaterialTheme.typography.bodyMedium)
            if (app.shortVersion != null && app.build != null && app.build != app.shortVersion) {
                Spacer(Modifier.height(12.dp))
                Text("Build", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Text(app.build, style = MaterialTheme.typography.bodyMedium)
            }
        }
        Spacer(Modifier.height(12.dp))
        Text(when {
            lookup == null && installed.lookupError == null -> "Looking for this app in the Legacy Store…"
            lookup == null -> "Couldn't check the Legacy Store: ${installed.lookupError}"
            else -> "The Legacy Store doesn't have this app, so there is no page for it and no newer version to offer."
        }, Modifier.padding(horizontal = 4.dp), style = MaterialTheme.typography.bodySmall,
            color = if (lookup == null && installed.lookupError != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
        if (lookup == null && installed.lookupError != null) TextButton(onClick = { installed.lookUp() }) { Text("Try again") }
        Spacer(Modifier.weight(1f))
        RemoveApp(model, app.bundleId, app.title, model.queuedBundle(app.bundleId)?.active != true)
        Spacer(Modifier.height(12.dp))
    }
}
