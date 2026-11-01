package org.lighttouch.android

import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import android.os.Handler
import android.os.Looper
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Search
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.util.Locale

/** A JSON string field, or null when absent, empty or JSON null (optString would return "null"). */
internal fun JSONObject.text(key: String): String? = if (isNull(key)) null else optString(key).ifEmpty { null }

internal fun productName(productType: String): String = when (productType) {
    "iPod1,1" -> "iPod touch"; "iPod2,1" -> "iPod touch (2nd generation)"; "iPod3,1" -> "iPod touch (3rd generation)"
    "iPod4,1" -> "iPod touch (4th generation)"; "iPhone1,1" -> "iPhone"; "iPhone1,2" -> "iPhone 3G"; "iPhone2,1" -> "iPhone 3GS"
    "iPhone3,1" -> "iPhone 4"; "iPad1,1" -> "iPad"; else -> productType
}

internal fun byteSize(bytes: Long): String = when {
    bytes >= 1L shl 30 -> String.format(Locale.ROOT, "%.1f GB", bytes / 1073741824.0)
    bytes >= 1L shl 20 -> String.format(Locale.ROOT, "%.1f MB", bytes / 1048576.0)
    else -> String.format(Locale.ROOT, "%d KB", bytes / 1024)
}

/**
 * Legacy Store state. Network and guest-service work runs on plain worker threads and posts
 * back to the main thread, like the rest of the activity. [muxSocket] is the current worker's
 * usbmux socket, or null while no guest is running. Installs belong to the process-wide [StoreInstaller],
 * not to this model (the activity, and with it the model, can be recreated mid-install);
 * [askNotifications] runs when the first app joins an empty queue.
 */
internal class StoreModel(context: Context, private val muxSocket: () -> String?, private val askNotifications: () -> Unit = {}) {
    private val handler = Handler(Looper.getMainLooper())
    private val apps = GuestApps(context)
    private val appContext = context.applicationContext
    private val installer = StoreInstaller.of(context)
    var target by mutableStateOf<StoreTarget?>(null)
    var targetConfirmed by mutableStateOf(false)
    var query by mutableStateOf("")
    var results by mutableStateOf<List<JSONObject>>(emptyList())
    var showIncompatible by mutableStateOf(false)
    var searching by mutableStateOf(false)
    var message by mutableStateOf("")
    var chosen by mutableStateOf<JSONObject?>(null)
    var copies by mutableStateOf<List<StoreCopy>?>(null)
    var selected by mutableStateOf<Int?>(null)
    var problem by mutableStateOf<String?>(null)
    var candidate by mutableStateOf<JSONObject?>(null)
    /** Bundle id to version, for the "Installed" labels on store pages. [installedList] has the whole list. */
    var installed by mutableStateOf<Map<String, String>>(emptyMap())
    val installedList = InstalledList(apps, muxSocket, { target }, { targetConfirmed }) { list ->
        installed = list.associate { it.bundleId to (it.version ?: "unknown") }
    }
    val browser = StoreBrowser { target }
    val resultsList = LazyListState()
    /** What the catalog says about the open app, and its newest screenshots; both arrive after the versions. */
    var info by mutableStateOf<JSONObject?>(null)
    var shots by mutableStateOf<List<StoreShot>>(emptyList())
    var queue by mutableStateOf(installer.queue.snapshot())
    var searchOpen by mutableStateOf(false)
    var crash by mutableStateOf<String?>(null)
    /** The bundle id being removed from the guest, and what went wrong with the last removal. */
    var removing by mutableStateOf<String?>(null)
    var removeError by mutableStateOf<String?>(null)
    private var searchGeneration = 0
    private var checkGeneration = 0

    val servicesAvailable get() = apps.available
    val installing get() = queue.any { it.active }

    init {
        installer.onChanged = { queue = it }
        installer.onInstalled = { installedList.set(it) }
    }

    private fun background(name: String, work: () -> Unit) = Thread(work, name).start()
    private fun post(work: () -> Unit) = handler.post(work)

    /** The device record the store is for: the installed apps and the confirmed target are that device's alone. */
    private var recordId = ""

    /**
     * Target from the selected device record, until a running guest reports its own. Another record starts over,
     * even of the same model: two iPod touch 2Gs on different iOS versions have different apps.
     */
    fun useRecord(id: String, next: StoreTarget?) {
        if (id != recordId) { recordId = id; installed = emptyMap(); installedList.reset(); targetConfirmed = false; target = null; results = emptyList(); chosen = null }
        if (next == target || (targetConfirmed && next?.productType == target?.productType)) return
        target = next; targetConfirmed = false; results = emptyList(); chosen = null; installedList.forgetLookups()
        if (next != null) search()
    }

    /** Another guest (or none) is behind the usbmux socket: what the last one reported no longer holds. */
    fun guestChanged() { installed = emptyMap(); installedList.guestChanged(); targetConfirmed = false; installer.socket = muxSocket() }

    /** Ask the running guest for its exact model and iOS version, and which apps it already has. */
    fun refreshGuest() {
        val socket = muxSocket() ?: return
        if (!apps.available) return
        background("store-guest") {
            try {
                val exact = apps.target(socket)
                val list = apps.installedApps(socket)
                post {
                    if (muxSocket() != socket) return@post  // a different guest answered by now
                    installedList.set(list)
                    if (exact != target) { target = exact; targetConfirmed = true; installedList.forgetLookups(); installedList.lookUp(); search() } else { targetConfirmed = true; installedList.lookUp() }
                }
            } catch (e: Exception) { post { if (!installing) message = "Guest app services: ${e.message}" } }
        }
    }

    fun search() {
        val t = target ?: run { message = "Select a device first."; return }
        val q = query; val generation = ++searchGeneration
        browser.tab = StoreTab.DEVICE
        searching = true; message = ""
        background("store-search") {
            val outcome = runCatching { LegacyStore.search(q, t) }
            post {
                if (generation != searchGeneration) return@post
                searching = false
                outcome.onSuccess { results = it; if (it.isEmpty()) message = "No apps found." }
                    .onFailure { message = it.message ?: "Search failed" }
            }
        }
    }

    fun open(app: JSONObject) {
        val t = target ?: return
        chosen = app; copies = null; selected = null; problem = null; candidate = null; info = null; shots = emptyList()
        val generation = ++checkGeneration
        background("store-info") { runCatching { LegacyStore.appInfo(app) }.getOrNull()?.let { found -> post { if (chosen === app) info = found } } }
        background("store-shots") { runCatching { LegacyStore.screenshots(app) }.getOrNull()?.let { found -> post { if (chosen === app) shots = found } } }
        background("store-versions") {
            val outcome = runCatching { LegacyStore.versions(app, t) }
            post {
                if (generation != checkGeneration) return@post
                outcome.onSuccess { rows ->
                    copies = rows
                    if (rows.isEmpty()) problem = "No version of this app runs on this device."
                    else choose(rows.firstOrNull { it.ipaId == LegacyStore.preferred(app.optInt("ipa_id")) }?.ipaId ?: rows[0].ipaId)
                }.onFailure { copies = emptyList(); problem = "Couldn't load versions: ${it.message}" }
            }
        }
    }

    fun close() { chosen = null; checkGeneration++ }

    /** An installed app's page: the store's when the catalog knows the app, else what the device says about it. */
    fun openInstalled(app: InstalledApp) {
        val found = (installedList.lookups[app.bundleId] as? Lookup.Found)?.known
        if (found != null) open(found.app) else installedList.plain = app
    }

    /** Remove an app and its data from the running guest, then read its list again and leave the app's page. */
    fun remove(bundleId: String) {
        val socket = muxSocket() ?: return
        if (removing != null || queuedBundle(bundleId)?.active == true) return
        removing = bundleId; removeError = null
        background("store-remove") {
            val failure = runCatching { apps.uninstall(socket, bundleId) }.exceptionOrNull()
            post {
                removing = null
                if (muxSocket() != socket) return@post
                if (failure != null) { removeError = "Couldn't remove it: ${failure.message}"; return@post }
                installed = installed - bundleId
                installedList.list?.let { list -> installedList.set(list.filter { it.bundleId != bundleId }) }
                installedList.refresh(force = true)
                close(); installedList.plain = null
            }
        }
    }

    /** The app's queue entry by bundle id, for lists that don't have the store's row. */
    fun queuedBundle(bundleId: String) = queue.firstOrNull { it.key == "app:$bundleId" }

    /** Revalidate one archived copy against this device (CatalogDetailsModel.check). */
    fun choose(ipaId: Int) {
        val t = target ?: return
        val app = chosen ?: return
        selected = ipaId; problem = null; candidate = null
        val generation = ++checkGeneration
        background("store-copy-check") {
            val outcome = runCatching {
                val found = LegacyStore.compatibleCopy(ipaId, t)
                // Rows from the charts and categories may not name a bundle.
                if (app.text("bundle_id")?.let { it != found.optString("bundle_id") } == true) throw StoreError("This copy belongs to a different app.")
                LegacyStore.checkCopy(LegacyStore.copyDetails(ipaId), found, t)
                found
            }
            post {
                if (generation != checkGeneration) return@post
                outcome.onSuccess { candidate = it }.onFailure { problem = it.message }
            }
        }
    }

    /** An app's place in the queue: by bundle, since two versions of one app can't install together. */
    private fun queueKey(app: JSONObject) = "app:" + (app.text("bundle_id") ?: app.text("app_store_id") ?: app.text("app_url")?.trimEnd('/')?.substringAfterLast('/') ?: app.optInt("ipa_id").toString())

    /** The app's queue entry, until the user dismisses it. */
    fun queued(app: JSONObject) = queueKey(app).let { key -> queue.firstOrNull { it.key == key } }

    private fun enqueue(key: String, name: String, request: InstallRequest) {
        val socket = muxSocket() ?: run { message = "Start the guest to install apps."; return }
        installer.socket = socket
        if (!installing) askNotifications()
        installer.queue.enqueue(key, name, request)
    }

    fun install(app: JSONObject) = enqueue(queueKey(app), app.optString("name"), InstallRequest.Catalog(app))
    fun installLocal(uri: Uri) = enqueue("local:$uri", "Local IPA", InstallRequest.Local(uri))

    /** While a launch is being recorded: the capture owns the relay, so a second one is refused. */
    var watching by mutableStateOf(false)
        private set

    /** Where the running device's own logs are (the record the store is showing). */
    private fun stateDir() = File(appContext.filesDir, "library/records/$recordId/state")

    /**
     * Why the last app quit. The crash report's faulting-thread frames say where it died (the old report kept only
     * the header, so every crash looked like the same null dereference), and the emulation's own logs say whether
     * the system or our side came first.
     */
    fun showLastCrash() {
        val socket = muxSocket() ?: run { crash = "Start the device to read its crash reports."; return }
        crash = "Reading crash reports…"
        background("store-crash") {
            val text = try {
                val last = runCatching { apps.lastCrash(socket) }.getOrNull()
                GuestCrashDiagnosis.report(stateDir(), last?.first, last?.second, null)
            } catch (e: Exception) { "Couldn't read crash reports: ${e.message}" }
            post { crash = text }
        }
    }

    /**
     * The crash report is written after the fact, the system log is not: it has to be listened to while the app
     * starts. Records 30 s of it, then reads the report and the logs into the same answer.
     */
    fun watchLaunch() {
        if (watching) return
        val socket = muxSocket() ?: run { crash = "Start the device to record its system log."; return }
        watching = true
        crash = "Recording the system log for 30 seconds. Open the app now and wait for it to quit."
        background("store-launch-log") {
            var log: String? = null
            var failure: String? = null
            try { log = apps.guestLog(socket, 30) } catch (e: Exception) { failure = e.message }
            val last = runCatching { apps.lastCrash(socket) }.getOrNull()
            val text = GuestCrashDiagnosis.report(stateDir(), last?.first, last?.second, log)
            post {
                watching = false
                crash = if (failure != null && log == null) "Couldn't record the system log ($failure).\n\n$text" else text
            }
        }
    }

    /** Collapse the search field; a search that was typed is cleared, and the device's own list comes back. */
    fun closeSearch() {
        searchOpen = false
        if (query.isNotBlank()) { query = ""; search() }
    }

    fun cancel(id: Long) = installer.queue.cancel(id)
    fun retry(id: Long) { installer.socket = muxSocket(); installer.queue.retry(id) }
    fun dismiss(id: Long) = installer.queue.dismiss(id)
    fun clearFinished() = installer.queue.clearFinished()
}

@Composable
internal fun AppIcon(url: String?, size: Int = 48, fallback: String? = null) {
    val bitmap by produceState<Bitmap?>(null, url) { value = url?.let { withContext(Dispatchers.IO) { LegacyStore.icon(it) } } }
    Box(Modifier.size(size.dp).clip(RoundedCornerShape((size / 4.5f).dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest)) {
        bitmap?.let { Image(it.asImageBitmap(), contentDescription = null, Modifier.fillMaxSize()) }
        // No icon to be had (the app isn't in the store): its initial, so the row still has something to tap.
        if (bitmap == null && url == null && !fallback.isNullOrBlank()) Text(fallback.trim().take(1).uppercase(), Modifier.align(Alignment.Center),
            style = if (size >= 64) MaterialTheme.typography.headlineSmall else MaterialTheme.typography.titleLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** Full-screen Legacy Store: search, compatibility, version picker and install progress. */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
internal fun StoreScreen(model: StoreModel, running: Boolean, onPickLocal: () -> Unit, onClose: () -> Unit) {
    val plain = model.installedList.plain
    BackHandler(enabled = model.chosen == null && plain == null, onBack = onClose)
    val details = model.chosen
    // Another device (or a guest reporting its exact iOS version) has other apps and other charts.
    LaunchedEffect(model.target) { model.browser.reset(); model.browser.show(model.browser.tab) }
    // Drawn over the activity's content (not a Dialog window, which gets no usable insets edge to edge).
    Surface(Modifier.fillMaxSize().testTag("store").semantics { testTagsAsResourceId = true }, color = MaterialTheme.colorScheme.background) {
        Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            StoreTopBar(model, running, onPickLocal, canSearch = details == null && plain == null, onBack = {
                when { details != null -> model.close(); plain != null -> model.installedList.plain = null; model.searchOpen -> model.closeSearch(); model.browser.canGoBack -> model.browser.back(); else -> onClose() }
            })
            Column(Modifier.padding(horizontal = 16.dp)) {
                model.crash?.let { report ->
                    val clipboard = LocalClipboardManager.current
                    StoreCard(Modifier.testTag("store_crash_report")) {
                        Text(if (model.watching) "Recording the launch…" else "Why it quit", style = MaterialTheme.typography.titleSmall)
                        Spacer(Modifier.height(6.dp))
                        Text(report, style = MonoStyle, modifier = Modifier.heightIn(max = 320.dp).verticalScroll(rememberScrollState()))
                        Row {
                            TextButton(onClick = { clipboard.setText(AnnotatedString(report)) }) { Text("Copy") }
                            Spacer(Modifier.weight(1f))
                            TextButton(onClick = { model.crash = null }) { Text("Close") }
                        }
                    }
                }
            }
            if (details != null) {
                BackHandler { model.close() }
                StoreDetails(model, details, running)
            } else if (plain != null) {
                BackHandler { model.installedList.plain = null }
                InstalledPlainPage(model, plain)
            } else {
                BrowseTabs(model.browser)
                BackHandler(enabled = model.browser.canGoBack) { model.browser.back() }
                BackHandler(enabled = model.searchOpen) { model.closeSearch() }
                when (model.browser.tab) {
                    StoreTab.DEVICE -> DeviceTab(model)
                    StoreTab.INSTALLED -> InstalledTab(model, running)
                    StoreTab.FEATURED -> FeaturedTab(model)
                    StoreTab.CHARTS -> ChartsTab(model)
                    StoreTab.CATEGORIES -> CategoriesTab(model)
                    StoreTab.COLLECTIONS -> CollectionsTab(model)
                }
            }
        }
    }
}

/** The apps the catalog has checked against this device, and search results (which also show the ones that won't run). */
@Composable
private fun ColumnScope.DeviceTab(model: StoreModel) {
    Row(Modifier.padding(horizontal = 16.dp), verticalAlignment = Alignment.CenterVertically) {
        FilterChip(model.showIncompatible, { model.showIncompatible = !model.showIncompatible }, label = { Text("Include apps that won't run") },
            leadingIcon = if (model.showIncompatible) {{ Icon(Icons.Filled.Check, null, Modifier.size(18.dp)) }} else null)
    }
    if (model.searching) LinearProgressIndicator(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp))
    if (model.message.isNotEmpty()) Text(model.message, Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    val rows = if (model.showIncompatible) model.results else model.results.filter { LegacyStore.incompatibility(it) == null }
    if (rows.isEmpty() && !model.searching) EmptyResults(model.results.isNotEmpty())
    else LazyColumn(Modifier.weight(1f).testTag("store_results"), state = model.resultsList, contentPadding = PaddingValues(horizontal = 8.dp, vertical = 4.dp)) {
        items(rows, key = { it.optInt("ipa_id") }) { app -> ResultRow(app, model) }
    }
}

@Composable
private fun StoreTopBar(model: StoreModel, running: Boolean, onPickLocal: () -> Unit, canSearch: Boolean, onBack: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    val searching = model.searchOpen && canSearch
    Row(Modifier.fillMaxWidth().height(64.dp).padding(horizontal = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = onBack, modifier = Modifier.testTag("store_close")) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
        if (searching) SearchField(model, Modifier.weight(1f))
        else {
            Column(Modifier.weight(1f)) {
                Text("Legacy Store", style = MaterialTheme.typography.titleLarge)
                Text(model.target?.let { "${shortProductName(it.productType)} · iOS ${it.ios}" } ?: "No device selected",
                    style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            if (canSearch) IconButton(onClick = { model.searchOpen = true }, modifier = Modifier.testTag("store_search")) { Icon(Icons.Filled.Search, "Search") }
        }
        QueueButton(model)
        Box {
            IconButton(onClick = { menu = true }, modifier = Modifier.testTag("store_menu")) { Icon(Icons.Filled.MoreVert, "More") }
            DropdownMenu(menu, { menu = false }) {
                DropdownMenuItem(text = { Text("Install an IPA file…") }, leadingIcon = { Icon(LtIcons.File, null) },
                    onClick = { menu = false; onPickLocal() }, enabled = running && model.servicesAvailable)
                DropdownMenuItem(text = { Text(if (model.watching) "Recording the launch…" else "Why did the last app quit?") },
                    leadingIcon = { Icon(LtIcons.Info, null) },
                    onClick = { menu = false; model.showLastCrash() }, enabled = running && model.servicesAvailable && !model.watching,
                    modifier = Modifier.testTag("store_crash"))
                DropdownMenuItem(text = { Text("Record the next app launch (30 s)") }, leadingIcon = { Icon(LtIcons.Info, null) },
                    onClick = { menu = false; model.watchLaunch() }, enabled = running && model.servicesAvailable && !model.watching,
                    modifier = Modifier.testTag("store_watch_launch"))
            }
        }
    }
}

/** The search field that takes the title's place in the top bar; Search runs it, Back closes it. */
@Composable
private fun SearchField(model: StoreModel, modifier: Modifier) {
    val keyboard = LocalSoftwareKeyboardController.current
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val search = { if (!model.searching && model.target != null) { keyboard?.hide(); model.search() } }
    TextField(model.query, { model.query = it }, modifier.padding(end = 4.dp).focusRequester(focus).testTag("store_query"),
        singleLine = true, shape = RoundedCornerShape(28.dp), placeholder = { Text("Search apps and games") },
        trailingIcon = {
            if (model.query.isNotEmpty()) IconButton(onClick = { model.query = "" }) { Icon(Icons.Filled.Close, "Clear") }
        },
        colors = TextFieldDefaults.colors(focusedIndicatorColor = Color.Transparent, unfocusedIndicatorColor = Color.Transparent, disabledIndicatorColor = Color.Transparent,
            focusedContainerColor = MaterialTheme.colorScheme.surfaceContainer, unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainer),
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search), keyboardActions = KeyboardActions(onSearch = { search() }))
}

@Composable
private fun ColumnScope.EmptyResults(filtered: Boolean) {
    Column(Modifier.weight(1f).fillMaxWidth().padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.Center) {
        Icon(LtIcons.Bag, null, Modifier.size(40.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.height(12.dp))
        Text(if (filtered) "Nothing here runs on this device" else "Find apps and games", style = MaterialTheme.typography.titleMedium)
        Text(if (filtered) "Include apps that won't run to see every result." else "Search the archive of classic iOS apps. Versions that run on this device are picked for you.",
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
    }
}

/** One app in a list. [rank] is its chart position. Rows from the plain listings have no compatibility verdict until opened. */
@Composable
internal fun ResultRow(app: JSONObject, model: StoreModel, rank: Int? = null) {
    if (app.optBoolean("ghost")) {
        Row(Modifier.fillMaxWidth().alpha(0.5f).padding(horizontal = 8.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Column {
                Text(app.optString("name"), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleSmall)
                Text("Not in the archive", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        return
    }
    val problem = LegacyStore.incompatibility(app)
    val installedVersion = model.installed[app.optString("bundle_id")]
    Row(Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).clickable { model.open(app) }.padding(horizontal = 8.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically) {
        rank?.let { Text(it.toString(), Modifier.width(32.dp), style = MaterialTheme.typography.titleSmall, textAlign = TextAlign.Center); Spacer(Modifier.width(8.dp)) }
        Box(Modifier.alpha(if (problem == null) 1f else 0.5f)) { AppIcon(app.optString("icon_url").ifEmpty { null }, 52) }
        Spacer(Modifier.width(14.dp))
        Column(Modifier.weight(1f)) {
            Text(app.optString("name"), maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.titleSmall,
                color = if (problem == null) Color.Unspecified else MaterialTheme.colorScheme.onSurfaceVariant)
            Text(listOfNotNull(app.text("developer"), app.text("version"), app.text("label"),
                app.optLong("size").takeIf { it > 0 }?.let(::byteSize),
                app.optInt("version_count").takeIf { it > 0 && app.text("version") == null }?.let { if (it == 1) "1 version" else "$it versions" }).joinToString(" · "),
                maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            val inQueue = model.queued(app)?.takeIf { it.active }
            when {
                problem != null -> Text(problem, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error, maxLines = 2)
                inQueue != null -> Text(if (inQueue.state == QueueState.PENDING) "In the queue" else inQueue.phase, style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary, maxLines = 1)
                installedVersion != null -> Text("Installed $installedVersion", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
            }
        }
    }
}

@Composable
private fun ColumnScope.StoreDetails(model: StoreModel, app: JSONObject, running: Boolean) {
    val installedVersion = model.installed[app.optString("bundle_id")]
    Column(Modifier.weight(1f).padding(horizontal = 16.dp)) {
        Row(Modifier.padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            AppIcon(app.optString("icon_url").ifEmpty { null }, 72)
            Spacer(Modifier.width(16.dp))
            Column {
                Text(app.optString("name"), style = MaterialTheme.typography.titleLarge, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Text(app.text("developer") ?: model.info?.text("developer") ?: "", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                model.info?.let { info -> listOfNotNull(info.text("genre"), info.text("original_release_date")?.take(4)).takeIf { it.isNotEmpty() }?.let {
                    Text(it.joinToString(" · "), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                } }
                installedVersion?.let { Text("Installed $it", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary) }
            }
        }
        if (model.shots.isNotEmpty()) Screenshots(model.shots)
        Text("Versions that run on this device", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(start = 4.dp, top = 12.dp, bottom = 8.dp))
        val rows = model.copies
        Column(Modifier.weight(1f, fill = false).fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainer)) {
            if (rows == null) LinearProgressIndicator(Modifier.fillMaxWidth().padding(16.dp))
            else if (rows.isEmpty()) Text("None found.", Modifier.padding(16.dp), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            // The copy selected first (the search row's, or its stand-in) can sit far down the list: show it.
            val versionList = rememberLazyListState()
            LaunchedEffect(rows) { rows?.indexOfFirst { it.ipaId == model.selected }?.takeIf { it > 0 }?.let { versionList.scrollToItem(it) } }
            LazyColumn(Modifier.testTag("store_versions"), state = versionList, contentPadding = PaddingValues(vertical = 6.dp)) {
                items(rows ?: emptyList(), key = { it.ipaId }) { copy ->
                    val twin = (rows ?: emptyList()).count { it.version == copy.version } > 1
                    Row(Modifier.fillMaxWidth().clickable { model.choose(copy.ipaId) }.padding(horizontal = 8.dp, vertical = 2.dp), verticalAlignment = Alignment.CenterVertically) {
                        RadioButton(model.selected == copy.ipaId, { model.choose(copy.ipaId) })
                        Column {
                            Text(copy.version + if (twin) " · copy ${copy.ipaId}" else "", style = MaterialTheme.typography.bodyLarge)
                            Text(listOfNotNull(copy.size?.let(::byteSize), copy.minimumOS?.let { "iOS $it or later" }).joinToString(" · "),
                                style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                }
            }
        }
        Spacer(Modifier.height(12.dp))
        val note = @Composable { text: String, error: Boolean ->
            Text(text, Modifier.padding(horizontal = 4.dp, vertical = 2.dp), style = MaterialTheme.typography.bodySmall,
                color = if (error) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
        }
        model.problem?.let { note(it, true) }
        val chosenVersion = rows?.firstOrNull { it.ipaId == model.selected }?.version
        if (installedVersion != null && chosenVersion != null) {
            val a = LegacyStore.version(chosenVersion.substringBefore(' ')); val b = LegacyStore.version(installedVersion)
            if (a != null && b != null && LegacyStore.newer(b, a)) note("Version $installedVersion is installed. An older version may not read its data.", false)
        }
        if (!running) note("Start the device to install.", false)
        else if (!model.servicesAvailable) note("App installation is not included in this build.", true)
        Spacer(Modifier.height(8.dp))
        val inQueue = model.queued(app)?.takeIf { it.active }
        Button(onClick = { model.candidate?.let { model.install(it); model.close() } }, Modifier.align(Alignment.CenterHorizontally).height(52.dp).testTag("store_install"),
            contentPadding = PaddingValues(horizontal = 40.dp), enabled = running && model.servicesAvailable && model.candidate != null && inQueue == null) {
            Text(if (inQueue != null) "In the queue" else if (installedVersion != null) "Reinstall" else "Install", style = MaterialTheme.typography.titleMedium)
        }
        if (installedVersion != null) RemoveApp(model, app.optString("bundle_id"), app.optString("name"), running && inQueue == null)
        Spacer(Modifier.height(12.dp))
    }
}

@Composable
private fun Screenshots(shots: List<StoreShot>) {
    LazyRow(Modifier.fillMaxWidth().padding(vertical = 8.dp).testTag("store_screenshots"), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        items(shots, key = { it.url }) { shot ->
            val bitmap by produceState<Bitmap?>(null, shot.url) { value = withContext(Dispatchers.IO) { LegacyStore.screenshot(shot.url) } }
            val ratio = if (shot.width > 0 && shot.height > 0) shot.width.toFloat() / shot.height else 0.6f
            Box(Modifier.height(190.dp).aspectRatio(ratio).clip(RoundedCornerShape(10.dp)).background(MaterialTheme.colorScheme.surfaceContainerHighest)) {
                bitmap?.let { Image(it.asImageBitmap(), contentDescription = null, Modifier.fillMaxSize()) }
            }
        }
    }
}

@Composable
private fun StoreCard(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) =
    Column(modifier.fillMaxWidth().padding(vertical = 6.dp).clip(RoundedCornerShape(20.dp)).background(MaterialTheme.colorScheme.surfaceContainerHigh)
        .padding(horizontal = 16.dp, vertical = 12.dp), content = content)

/**
 * How far along the whole queue is, 0 to 1: each item's [QueueItem.overall], which runs once through download, copy
 * onto the device and iOS's install (see [QueueStages]); finished items (installed or failed) count whole, until cleared.
 */
internal fun queueFraction(items: List<QueueItem>): Float {
    if (items.isEmpty()) return 0f
    return items.sumOf { item ->
        when (item.state) {
            QueueState.DONE, QueueState.FAILED -> 1.0
            else -> item.overall.coerceIn(0f, 1f).toDouble()
        }
    }.toFloat() / items.size
}

/** The install queue's button: a download icon in a progress ring, which opens the queue over the store. Absent while the queue is empty. */
@Composable
private fun QueueButton(model: StoreModel) {
    val items = model.queue
    if (items.isEmpty()) return
    var open by remember { mutableStateOf(false) }
    val busy = items.any { it.active }
    val failed = !busy && items.any { it.state == QueueState.FAILED }
    Box(Modifier.size(48.dp), contentAlignment = Alignment.Center) {
        IconButton(onClick = { open = true }, modifier = Modifier.testTag("store_queue")) {
            Icon(LtIcons.Download, if (busy) "Installs in progress" else "Installs", tint = if (failed) MaterialTheme.colorScheme.error else LocalContentColor.current)
        }
        if (busy) CircularProgressIndicator(progress = { queueFraction(items).coerceIn(0.02f, 1f) }, modifier = Modifier.size(40.dp), strokeWidth = 3.dp)
        QueueMenu(model, open) { open = false }
    }
}

/** The queue as a floating list under its button: each app's progress, Cancel or Retry, and Clear finished. */
@Composable
private fun QueueMenu(model: StoreModel, open: Boolean, onDismiss: () -> Unit) {
    val items = model.queue
    DropdownMenu(open, onDismiss, Modifier.widthIn(min = 300.dp, max = 340.dp), shape = MaterialTheme.shapes.large,
        containerColor = MaterialTheme.colorScheme.surfaceContainerHigh) {
        Row(Modifier.padding(start = 16.dp, end = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(if (items.any { it.active }) "Installing" else "Installs finished", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
            if (items.any { !it.active }) TextButton(onClick = { model.clearFinished() }) { Text("Clear finished") }
        }
        Column(Modifier.heightIn(max = 320.dp).verticalScroll(rememberScrollState()).padding(horizontal = 16.dp)) {
            for (item in items) QueueRow(item, model)
        }
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
private fun QueueProgress(item: QueueItem) {
    if (item.progress != null) LinearProgressIndicator(progress = { item.progress.coerceIn(0f, 1f) }, modifier = Modifier.fillMaxWidth().padding(top = 8.dp))
    else LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 8.dp))
}

@Composable
private fun QueueRow(item: QueueItem, model: StoreModel) {
    Column(Modifier.fillMaxWidth().padding(top = 8.dp).testTag("store_job")) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text(item.name, style = MaterialTheme.typography.bodyMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(item.error ?: item.phase, style = MaterialTheme.typography.bodySmall, maxLines = 2,
                    color = if (item.error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant)
            }
            if (item.cancellable) TextButton(onClick = { model.cancel(item.id) }) { Text("Cancel") }
            if (item.state == QueueState.FAILED) TextButton(onClick = { model.retry(item.id) }) { Text("Retry") }
        }
        if (item.state == QueueState.DOWNLOADING || item.state == QueueState.INSTALLING) QueueProgress(item)
    }
}

/** Remove an installed app from the device, after asking. */
@Composable
internal fun ColumnScope.RemoveApp(model: StoreModel, bundleId: String, name: String, enabled: Boolean) {
    var asking by remember { mutableStateOf(false) }
    val busy = model.removing == bundleId
    model.removeError?.let { Text(it, Modifier.padding(horizontal = 4.dp, vertical = 2.dp), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
    TextButton(onClick = { asking = true }, Modifier.align(Alignment.CenterHorizontally).testTag("store_remove"),
        enabled = enabled && model.servicesAvailable && model.removing == null, colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error)) {
        Text(if (busy) "Removing…" else "Remove from device")
    }
    if (asking) AlertDialog(onDismissRequest = { asking = false }, modifier = DialogTags, icon = { Icon(Icons.Filled.Delete, null) },
        title = { Text("Remove $name?", style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center) },
        text = { Text("The app and its data are deleted from the device. You can install it again from the Legacy Store.") },
        confirmButton = { Button(onClick = { asking = false; model.remove(bundleId) },
            colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error), modifier = Modifier.testTag("confirm_remove")) { Text("Remove") } },
        dismissButton = { TextButton(onClick = { asking = false }) { Text("Cancel") } })
}
