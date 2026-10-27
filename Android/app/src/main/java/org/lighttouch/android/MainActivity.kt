package org.lighttouch.android

import android.content.*
import android.hardware.*
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.MediaStore
import org.json.JSONObject
import android.os.*
import android.content.pm.ActivityInfo
import android.view.KeyEvent
import android.view.WindowInsets as PlatformInsets
import android.view.WindowInsetsController
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.*
import androidx.compose.ui.unit.dp
import androidx.core.view.WindowCompat
import androidx.documentfile.provider.DocumentFile
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.Executors

@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
class MainActivity : ComponentActivity(), SensorEventListener, GuestHost {
    private var engine by mutableStateOf<IEngine?>(null)
    override var status by mutableStateOf("stopped")
        private set
    private var detail by mutableStateOf("")
    override var displayAsleep by mutableStateOf(false)
        private set
    override var error by mutableStateOf("")
        private set
    private val workerEvidenceDelivery = SessionDeliveryGate()
    private var workerPid by mutableIntStateOf(0)
    override var sessionOwned by mutableStateOf(false)
        private set
    private data class GuestStartRequest(val recordId: String, val coldRecovery: Boolean)
    private var pendingGuestRequest: GuestStartRequest? = null
    private var pendingGuestStart = false
    private var guestStartRequested = false
    private var engineExpectedExit = false
    private var engineRestarting = false
    override var busy by mutableStateOf(false)
        private set
    /** A passing message (screenshot saved, device renamed), shown as a snackbar. */
    private var notice by mutableStateOf("")
    override var profileRecording by mutableStateOf(false)
        private set
    override var profileAutomatic by mutableStateOf(false)
        private set
    override var profileBusy by mutableStateOf(false)
        private set
    override var profileNotice by mutableStateOf("")
        private set
    private var licensesOpen by mutableStateOf(false)
    private var devicesOpen by mutableStateOf(false)
    private var detailsDevice by mutableStateOf<DeviceSummary?>(null)
    private var muxSocket by mutableStateOf("")
    private var appServicesError by mutableStateOf("")
    override var networkSessionStatus by mutableStateOf(NetworkSessionStatus())
        private set
    override var networkChangeNotice by mutableStateOf("")
        private set
    private val installer by lazy { StoreInstaller.of(applicationContext) }
    private val store by lazy { StoreModel(applicationContext, { muxSocket.ifEmpty { null } }, ::askNotifications) }
    // Android 13+ shows an install queue's progress and result in notifications only once allowed.
    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }
    private fun askNotifications() {
        if (Build.VERSION.SDK_INT >= 33 && !askedNotifications && checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            askedNotifications = true
            notificationPermission.launch(android.Manifest.permission.POST_NOTIFICATIONS)
        }
    }
    private val localIpa = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> if (uri != null) store.installLocal(uri) }
    // board-build -> (product type, iOS version), from the bundled firmware catalog.
    @Volatile private var firmwareTargets: Map<String, StoreTarget> = emptyMap()
    private var catalogVersion by mutableIntStateOf(0)
    // Hardware buttons are Binder calls; one thread keeps down/up ordered and off the UI thread.
    private val buttonCalls = Executors.newSingleThreadExecutor { Thread(it, "guest-buttons") }
    private var records by mutableStateOf<List<Bundle>>(emptyList())
    private var recordsLoaded by mutableStateOf(false)
    override val devices: List<DeviceSummary> get() { catalogVersion; return records.map { DeviceSummary.of(it, firmwareTargets) } }
    override val selected: DeviceSummary? get() = devices.firstOrNull { it.selected }
    override val settings by lazy { AppSettings(this) }
    private val monitor = PerformanceMonitor { engine }
    override val perf: PerfSnapshot get() = monitor.snapshot
    override val storeInstalling get() = store.installing
    // Save & Stop needs the engine to serialize live GL state on EGL hosts (docs/TODO.md).
    override val canSaveState = false
    /** HOME or FULLSCREEN: what is under any full-page overlay. */
    private var base by mutableStateOf(Screen.HOME)
    /** LOADING, SETUP, SETTINGS, STORE or HITCH drawn over [base]; null shows the guest. */
    private var overlay by mutableStateOf<Screen?>(Screen.LOADING)
    private val setup = SetupState()
    private var hitchData by mutableStateOf<HitchData?>(null)
    private var hitchError by mutableStateOf<String?>(null)
    private var hitchSaving by mutableStateOf(false)
    private var hitchNotice by mutableStateOf<String?>(null)
    private var hitchReport by mutableStateOf<Uri?>(null)
    private var hitchResume = false
    // The guest was running when LightTouch left the screen; resume it on return (settings.autoResume).
    private var pausedByBackground = false
    private var askedNotifications = false
    private val idleHook: () -> Unit = ::installsFinished
    private var firmware: IFirmware? = null
    private var firmwareBound = false
    private var firmwareRestarting by mutableStateOf(false)
    private var pendingFirmwareStart = false
    private var firmwareExpectedExit = false
    private var retainedPreparedOutput = ""
    private var preparedImportInFlight = false
    private var preparationOpen by mutableStateOf(false)
    private val prep = PrepareState()
    private var preparedImported = ""
    /** A preparation that finished while LightTouch was closed: added as soon as the device worker is up. */
    private var addRetainedWhenReady = false
    // A finished run the user went back from; the worker keeps reporting it until it is replaced.
    private var dismissedRun = ""
    private val firmwareConnection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            val worker = IFirmware.Stub.asInterface(binder)
            firmware = worker
            firmwareExpectedExit = false
            binder.linkToDeath({ handler.post {
                if (firmware?.asBinder() == binder) {
                    firmware = null
                    if (!firmwareRestarting && !firmwareExpectedExit) prep.preparation = workerExitedRun()
                }
            } }, 0)
            val workerState = worker.status()
            prep.preparation = if (workerState.getString("state") == "idle" && retainedPreparedOutput.isNotEmpty()) retainedPreparation() else workerState.takeIf { it.getString("state") != "idle" }
            if (pendingFirmwareStart) {
                pendingFirmwareStart = false
                firmwareRestarting = false
                if (workerState.getBoolean("consumed")) restartFirmwareForPreparation() else beginPreparation()
            }
        }
        override fun onServiceDisconnected(name: ComponentName) { firmware = null; if (!firmwareRestarting && !firmwareExpectedExit) prep.preparation = workerExitedRun() }
    }
    private fun workerExitedRun() = Bundle().apply { putString("state", "failed"); putString("error", "The preparation worker stopped unexpectedly. Try again.") }
    private val stockIpsw = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> if (uri != null) importStockIpsw(uri) }
    private val prepRom = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> if (uri != null) importPrepRom(uri) }
    private var active = false
    private var displayOwnerTicket = 0L
    private var displayResumed = false
    override fun toggleProfile() {
        val worker = engine ?: return
        val recording = !profileRecording
        profileBusy = true
        Thread({
            val result = try { worker.profile(recording) }
                catch (e: Exception) { Bundle().apply { putString("profileError", e.message ?: "Profile capture failed") } }
            handler.post {
                if (engine?.asBinder() == worker.asBinder()) {
                    profileRecording = result.getBoolean("profileRecording")
                    profileNotice = result.getString("profileNotice") ?: ""
                    result.getString("profileError")?.takeIf { it.isNotEmpty() }?.let { error = it }
                }
                profileBusy = false
            }
        }, "profile-control").start()
    }
    private fun ownsDisplay() = active && DisplayOwners.active.owns(this, displayOwnerTicket)
    private fun mayClaimDisplay() = active && displayResumed && DisplayOwners.active.canClaim(this, displayOwnerTicket)
    private var bound = false
    private var displayView: GuestDisplay? = null
    private val handler = Handler(Looper.getMainLooper())
    private val sensors by lazy { getSystemService(SensorManager::class.java) }
    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            workerEvidenceDelivery.advance()
            engine = IEngine.Stub.asInterface(binder)
            refreshDevices()
            applySettings()
            binder.linkToDeath({ handler.post { if (engine?.asBinder() == binder) workerExited() } }, 0)
            if (pendingGuestStart) {
                val worker = requireNotNull(engine)
                Thread({
                    try { val state = worker.status(); handler.post {
                        if (engine?.asBinder() != binder) return@post
                        sessionOwned = state.getBoolean("sessionStarted"); status = state.getString("state") ?: "stopped"
                        pendingGuestStart = false; engineRestarting = false
                        pendingGuestRequest?.let { startGuest(retainedRequest = it) }
                            ?: run { error = "Guest start request was lost; select Start or Recover retained state again."; status = "failed" }
                    } } catch (failure: RemoteException) { handler.post { pendingGuestStart = false; workerExited() } }
                }, "guest-reconnect").start()
            }
            displayView?.attachActiveSurface()
        }
        override fun onServiceDisconnected(name: ComponentName) { workerExited() }
    }
    private var pollInFlight = false
    private val poll = object : Runnable {
        override fun run() {
            if (!active || pollInFlight) return
            pollInFlight = true
            val observedEngine = engine
            val observedFirmware = if (preparationOpen && !firmwareRestarting) firmware else null
            Thread({
                var engineState: Bundle? = null
                var firmwareState: Bundle? = null
                var engineFailure: String? = null
                var firmwareFailure: String? = null
                try { engineState = observedEngine?.status() } catch (e: RemoteException) { engineFailure = e.message ?: "Worker exited" }
                try { firmwareState = observedFirmware?.status() } catch (e: RemoteException) { firmwareFailure = e.message ?: "Firmware worker exited" }
                handler.post {
                    pollInFlight = false
                    if (!active) return@post
                    if (engine?.asBinder() == observedEngine?.asBinder()) {
                        engineState?.let { b ->
                            status = b.getString("state") ?: "Unknown"
                            detail = b.getString("detail") ?: ""
                            displayAsleep = b.getBundle("observations")?.let {
                                it.getBoolean("runtimeDiagnosticsAvailable") && it.getLong("engineReady") != 0L && it.getLong("displaySleeping") != 0L
                            } ?: false
                            val reportedError = b.getString("profileError")?.takeIf { it.isNotEmpty() } ?: b.getString("error") ?: ""
                            if (reportedError != dismissedError) dismissedError = ""
                            if (reportedError.isNotEmpty() || status == "running") error = if (reportedError == dismissedError) "" else reportedError
                            workerPid = b.getInt("pid")
                            profileRecording = b.getBoolean("profileRecording"); profileAutomatic = b.getBoolean("profileAutomatic")
                            profileNotice = b.getString("profileNotice") ?: ""
                            sessionOwned = b.getBoolean("sessionStarted")
                            val socket = if (status == "running") b.getString("usbMuxSocket") ?: "" else ""
                            if (socket != muxSocket) { muxSocket = socket; store.guestChanged(); if (socket.isNotEmpty() && overlay == Screen.STORE) store.refreshGuest() }
                            appServicesError = b.getString("appServicesError") ?: ""
                            networkSessionStatus = NetworkSessionStatus(
                                available = b.containsKey("networkSetup"),
                                mode = b.getInt("networkMode"),
                                archiveDate = b.getString("networkArchiveDate").orEmpty(),
                                setup = b.getString("networkSetup").orEmpty(),
                                setupError = b.getBoolean("networkSetupError"),
                                activity = b.getString("networkActivity").orEmpty(),
                                connections = b.getLong("networkConnections"),
                                completed = b.getLong("networkCompleted"),
                                failed = b.getLong("networkFailed"),
                                lastResult = b.getString("networkLastResult").orEmpty(),
                                lastResultError = b.getBoolean("networkLastResultError"),
                            )
                            networkChangeNotice = b.getString("networkChangeNotice") ?: ""
                            displayView?.keepScreenOn = status in listOf("starting", "running", "stopping")
                            if (status == "running") startSensors() else stopSensors()
                        }
                        engineFailure?.let { workerExited(); if (!engineExpectedExit) error = "Guest worker unavailable: $it. Writable state retained; Start creates a fresh worker." }
                    }
                    // After the catalog too: it names the device.
                    if (addRetainedWhenReady && engine != null && prep.catalog.isNotEmpty() && !preparedImportInFlight && prep.preparation?.getString("state") == "complete") {
                        addRetainedWhenReady = false; prep.preparation?.let { importPreparedOutput(it) }
                    }
                    if (firmware?.asBinder() == observedFirmware?.asBinder() && !firmwareRestarting) {
                        firmwareState?.let { pollPreparation(it) }
                        firmwareFailure?.let { error = "Firmware worker unavailable: $it" }
                    }
                    handler.postDelayed(this, 500)
                }
            }, "worker-status").start()
        }
    }
    /**
     * Run the panel at 60 Hz while LightTouch is in front, the way Google Maps does. The guest is a
     * 60 Hz device: at 120 Hz each of its frames must also land on the right one of two refreshes,
     * which the presenter can only keep by tracking the display's phase (frame_pacer.h), and the
     * Pixel 10 kept 120 Hz despite the guest surface's 60 Hz fixed-source request. A preferred
     * display mode is the window's own request, which the system grants the foreground window
     * whatever the Smooth Display setting. Only a 60 Hz mode at the current resolution is taken.
     *
     * Touch boost (Android 15+) is off for the same reason: touch is constant in a game, and a boost
     * would raise the panel to its top rate. Both apply only while the guest is what is on screen;
     * the Store and the dialogs over it are ordinary scrolling UI, which at a fixed 60 Hz with no
     * boost looked choppy beside the rest of a 120 Hz phone, so they get the system's own choice.
     */
    private var sixtyHertzMode = -1
    private fun preferSixtyHertz(guestInFront: Boolean) {
        if (sixtyHertzMode < 0) {
            val current = (if (Build.VERSION.SDK_INT >= 30) display else @Suppress("DEPRECATION") windowManager.defaultDisplay) ?: return
            val size = current.mode
            sixtyHertzMode = current.supportedModes
                .filter { it.physicalWidth == size.physicalWidth && it.physicalHeight == size.physicalHeight }
                .minByOrNull { kotlin.math.abs(it.refreshRate - 60f) }
                ?.takeIf { kotlin.math.abs(it.refreshRate - 60f) < 1f }?.modeId ?: 0
        }
        val mode = if (guestInFront) sixtyHertzMode else 0
        if (window.attributes.preferredDisplayModeId != mode) window.attributes = window.attributes.apply { preferredDisplayModeId = mode }
        if (Build.VERSION.SDK_INT >= 35) window.setFrameRateBoostOnTouchEnabled(!guestInFront)
    }
    private val pickGuestToolsFile = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri -> if (uri != null) copyAsset(uri, "guest.itpack") }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The guest is a portrait device; its landscape apps rotate inside the portrait screen.
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        WindowCompat.setDecorFitsSystemWindows(window, false)
        // Draw under transparent bars; the theme picks light or dark bar icons.
        @Suppress("DEPRECATION") run { window.statusBarColor = android.graphics.Color.TRANSPARENT; window.navigationBarColor = android.graphics.Color.TRANSPARENT }
        window.isNavigationBarContrastEnforced = false; window.isStatusBarContrastEnforced = false
        preferSixtyHertz(guestInFront = false)
        installer.onIdle = idleHook
        // Retain the previous own-worker crash even when it happened before this
        // activity/process was running. Never request another application's trace.
        Thread({ WorkerExitEvidence.capture(applicationContext, 0) }, "previous-worker-exit").start()
        val imports = getSharedPreferences("preparation-imports", MODE_PRIVATE)
        imports.getString("ipsw", "")?.takeIf { File(it).isFile }?.let { prep.ipswPath = it; prep.ipswFor = imports.getString("ipswFor", "") ?: "" }
        prep.firmwareId = imports.getString("firmwareId", "") ?: ""
        preparedImported = imports.getString("preparedImported", "") ?: ""
        retainedPreparedOutput = imports.getString("pendingPrepared", "") ?: ""
        if (retainedPreparedOutput.isNotEmpty()) {
            prep.preparation = retainedPreparation(); preparedImported = retainedPreparedOutput
            prep.importError = "The prepared device is kept; try adding it again."
        } else resumePreparation()
        Thread({
            try {
                val catalog = FirmwareCatalog.load(this)
                firmwareTargets = catalog.associate { "${it.board}-${it.build}" to StoreTarget.of(it.productType, it.version) }
                handler.post { prep.catalog = catalog; catalogVersion++; if (overlay == Screen.STORE) store.useRecord(selectedRecordId(), recordTarget()); if (engine != null) refreshDevices() }
            } catch (_: Exception) { /* The Store falls back to the running guest's own report. */ }
        }, "store-targets").start()
        bindWorker()
        setContent {
            val guestInFront = overlay == null && !devicesOpen && !licensesOpen && !preparationOpen && status == "running"
            LaunchedEffect(guestInFront) { preferSixtyHertz(guestInFront) }
            // A full page hides the guest surface: the presenter stops drawing into it and drops its 60 Hz request.
            val guestCovered = overlay in listOf(Screen.LOADING, Screen.SETUP, Screen.SETTINGS, Screen.STORE, Screen.DETAILS)
            LaunchedEffect(guestCovered, engine) {
                val worker = engine ?: return@LaunchedEffect
                Thread({ try { worker.setPresentVisible(!guestCovered) } catch (_: RemoteException) { } }, "present-visibility").start()
            }
            val immersive = base == Screen.FULLSCREEN && overlay == null
            LaunchedEffect(immersive) { applyWindowMode(immersive) }
            // A guest that stops while full screen drops back to the device view.
            LaunchedEffect(status) { if (base == Screen.FULLSCREEN && status in listOf("stopped", "failed")) base = Screen.HOME }
            // First run: no devices yet, so set one up.
            LaunchedEffect(recordsLoaded, records.isEmpty()) {
                if (recordsLoaded && overlay == Screen.LOADING) overlay = if (records.isEmpty()) Screen.SETUP.also { setup.reset() } else null
            }
            val showOverlay = settings.overlay != OverlayLevel.OFF && status == "running" &&
                ((overlay == null && (base == Screen.FULLSCREEN || settings.overlayInDeviceView)) || overlay == Screen.SETTINGS)
            LaunchedEffect(showOverlay, settings.overlay) { monitor.setActive(showOverlay, settings.overlay == OverlayLevel.FULL) }
            LightTouchTheme(settings.theme) {
                val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
                LaunchedEffect(dark, immersive) {
                    WindowCompat.getInsetsController(window, window.decorView).apply {
                        isAppearanceLightStatusBars = !dark && !immersive; isAppearanceLightNavigationBars = !dark && !immersive
                    }
                }
                // One SurfaceView moves between the device and full-screen layouts, keeping its display lease.
                val guestSurface = remember { movableContentOf { modifier: Modifier ->
                    WorkerGuestSurface(worker = engine, currentWorker = { engine },
                        mayClaim = { view -> mayClaimDisplay() && displayView === view },
                        ownerTicket = { displayOwnerTicket }, onError = { error = it },
                        onCreated = { displayView = it },
                        onReleased = { view -> if (displayView === view) displayView = null },
                        keepScreenOn = active && status in listOf("starting", "running", "stopping"),
                        modifier = modifier.background(Color.Black).testTag("guest_display"))
                } }
                val snackbar = remember { SnackbarHostState() }
                LaunchedEffect(notice) { if (notice.isNotEmpty()) { snackbar.showSnackbar(notice); notice = "" } }
                Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
                    // The guest layout stays composed under full-page overlays so its Surface and lease survive them.
                    if (base == Screen.FULLSCREEN) FullscreenScreen(this@MainActivity, guestSurface) else HomeScreen(this@MainActivity, guestSurface)
                    when (overlay) {
                        Screen.LOADING -> Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background), contentAlignment = Alignment.Center) {
                            CircularProgressIndicator(Modifier.size(32.dp), strokeWidth = 3.dp)
                        }
                        Screen.SETUP -> SetupWizard(setup, canCancel = records.isNotEmpty(), onPrepare = { openPreparation() },
                            onFinish = { start -> overlay = null; if (start) startGuest() }, onCancel = { overlay = null })
                        Screen.SETTINGS -> SettingsScreen(this@MainActivity) { overlay = null }
                        Screen.DETAILS -> detailsDevice?.let { DeviceDetails(it) { overlay = null; openDevices() } }
                        Screen.STORE -> StoreScreen(store, running = status == "running" && muxSocket.isNotEmpty(),
                            onPickLocal = { localIpa.launch(arrayOf("application/octet-stream", "*/*")) }) { overlay = null }
                        Screen.HITCH -> HitchInspector(hitchData, hitchError, onResume = ::closeHitch, onSave = ::saveHitch, saving = hitchSaving,
                            savedNotice = hitchNotice, onShare = hitchReport?.let { uri -> { shareReport(uri) } })
                        else -> {}
                    }
                    if (preparationOpen) PrepareOverlay()
                    SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter).windowInsetsPadding(WindowInsets.safeDrawing).padding(bottom = 96.dp))
                }
                if (devicesOpen) DevicesSheet(this@MainActivity) { devicesOpen = false }
                if (licensesOpen) LicenseDialog { licensesOpen = false }
            }
        }
    }

    // GuestHost: the screens' requests.
    /** The message the user dismissed: the worker goes on reporting it each poll, and it stays hidden until it changes. */
    private var dismissedError = ""
    override fun dismissError() { dismissedError = error; error = "" }
    override fun start() = startGuest()
    override fun recover() = startGuest(coldRecovery = true)
    override fun stop() = stopGuest()
    override fun saveAndStop() = stopGuest()
    override fun setPaused(paused: Boolean) {
        val worker = engine ?: return
        if (paused) displayView?.cancelOwnedGesture()
        pausedByBackground = false
        try { worker.pause(paused) } catch (_: RemoteException) { error = "Guest worker unavailable" }
    }
    override fun button(id: Int, down: Boolean) = sendButton(id, down)
    override fun screenshot() = capture()
    override fun navigate(screen: Screen) {
        when (screen) {
            Screen.HOME, Screen.FULLSCREEN -> { base = screen; overlay = null }
            Screen.STORE -> openStore()
            else -> overlay = screen
        }
    }
    override fun openDevices() { refreshDevices(); devicesOpen = true }
    // A full page like Settings, so it sits inside the window's insets; Back returns to the devices list.
    override fun openDetails(device: DeviceSummary) { detailsDevice = device; devicesOpen = false; overlay = Screen.DETAILS }
    override fun addDevice() = openPreparation()
    override fun openLicenses() { licensesOpen = true }
    override fun openPreparation() {
        if (!preparationActive() && prep.preparation?.getString("state") != "complete")
            prep.step = if (prep.firmware != null && prep.ipswReady) PrepareStep.FILES else PrepareStep.DEVICE
        preparationOpen = true; bindFirmware()
    }
    override fun pickGuestTools() { pickGuestToolsFile.launch(arrayOf("application/octet-stream", "*/*")) }
    override fun applySettings() {
        if (!settings.tilt) stopSensors() else if (status == "running") startSensors()
        val worker = engine ?: return
        val scale = settings.scaling.native; val detailed = settings.detailedTiming
        Thread({ try { worker.setPresentScale(scale); worker.setDetailedTiming(detailed); worker.applyNetworkSettings() } catch (_: RemoteException) { } }, "apply-settings").start()
    }
    override fun selectDevice(id: String) {
        busy = true
        val worker = engine
        Thread({
            val result = try { worker?.select(id) ?: "Worker unavailable" } catch (e: RemoteException) { e.message ?: "Device selection failed" }
            handler.post { error = result; busy = false; refreshDevices() }
        }, "device-selection").start()
    }
    override fun renameDevice(id: String, name: String) {
        val worker = engine ?: return
        Thread({
            val result = try { worker.rename(id, name) } catch (e: RemoteException) { e.message ?: "Rename failed" }
            handler.post { if (result.isEmpty()) notice = "Renamed to $name" else error = result; refreshDevices() }
        }, "device-rename").start()
    }
    override fun deleteDevice(id: String) {
        val worker = engine ?: return
        busy = true
        Thread({
            val result = try { worker.remove(id) } catch (e: RemoteException) { e.message ?: "Delete failed" }
            handler.post { busy = false; if (result.isEmpty()) notice = "Device deleted" else error = result; refreshDevices(afterDelete = true) }
        }, "device-delete").start()
    }

    /** Pause the guest, freeze the frame history and open the inspector over it. */
    override fun openHitch() {
        val worker = engine ?: return
        if (status !in listOf("running", "paused")) return
        hitchResume = status == "running"
        hitchData = null; hitchError = null; hitchNotice = null; hitchReport = null
        overlay = Screen.HITCH
        val running = hitchResume
        Thread({
            try {
                // The app in front, for the report's name: the guest tools answer only while the guest runs, so ask
                // before pausing it.
                val app = if (running) try { worker.frontmostApp() } catch (_: RemoteException) { null } else null
                if (running) handler.post { if (overlay == Screen.HITCH) setPaused(true) }
                val snapshot = worker.hitchSnapshot().apply { app?.let { putAll(it) } }
                val data = HitchData.load(snapshot)
                handler.post { if (overlay == Screen.HITCH) hitchData = data }
            } catch (e: Exception) { handler.post { hitchError = e.message ?: "Frame history unavailable" } }
        }, "hitch-snapshot").start()
    }
    private fun closeHitch() {
        overlay = null; hitchData = null
        if (hitchResume) setPaused(false)
    }
    private fun saveHitch(from: Long, to: Long) {
        val data = hitchData ?: return
        hitchSaving = true
        Thread({
            try {
                val uri = saveHitchReport(applicationContext, data, from, to)
                handler.post { hitchSaving = false; hitchReport = uri; hitchNotice = "Saved to Download/LightTouch" }
            } catch (e: Exception) { handler.post { hitchSaving = false; hitchNotice = "Could not save: ${e.message}" } }
        }, "hitch-report").start()
    }
    private fun shareReport(uri: Uri) {
        startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("application/zip").putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION), "Share hitch report"))
    }

    private fun displayName(uri: Uri) = DocumentFile.fromSingleUri(this, uri)?.name ?: uri.lastPathSegment ?: "file"
    /** What the engine's import is doing, as shown under the progress bar. */
    private fun importPhaseText(phase: String) = when (phase) {
        "hashing" -> "Checking the file…"
        "extracting" -> "Unpacking the iOS system…"
        "packing" -> "Packing the iOS storage…"
        "verifying" -> "Checking the iOS system…"
        else -> "Setting up the iOS system…"
    }
    /** Shows the engine's import progress through [show] (on the main thread) until the returned stop function runs. */
    private fun watchImport(worker: IEngine, show: (String, Float) -> Unit): () -> Unit {
        val running = java.util.concurrent.atomic.AtomicBoolean(true)
        Thread({
            while (running.get()) {
                val p = try { worker.importProgress() } catch (_: Exception) { null }
                if (p != null && running.get()) {
                    val done = p.getLong("done"); val total = p.getLong("total")
                    val text = importPhaseText(p.getString("phase") ?: ""); val fraction = if (total > 0) (done.toFloat() / total) else -1f
                    handler.post { if (running.get()) show(text, fraction) }
                }
                Thread.sleep(250)
            }
        }, "import-progress").start()
        return { running.set(false) }
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        val volume = when (keyCode) { KeyEvent.KEYCODE_VOLUME_UP -> GuestButton.VOLUME_UP; KeyEvent.KEYCODE_VOLUME_DOWN -> GuestButton.VOLUME_DOWN; else -> -1 }
        if (volume >= 0 && settings.volumeKeys && status == "running" && overlay == null && !devicesOpen) {
            if (event.repeatCount == 0) sendButton(volume, true)
            return true
        }
        return super.onKeyDown(keyCode, event)
    }
    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        val volume = when (keyCode) { KeyEvent.KEYCODE_VOLUME_UP -> GuestButton.VOLUME_UP; KeyEvent.KEYCODE_VOLUME_DOWN -> GuestButton.VOLUME_DOWN; else -> -1 }
        if (volume >= 0 && settings.volumeKeys && status == "running" && overlay == null && !devicesOpen) { sendButton(volume, false); return true }
        return super.onKeyUp(keyCode, event)
    }
    // Prepare from an IPSW: the choices live in [prep]; one FirmwareService process runs one preparation.
    private fun retainedPreparation() = Bundle().apply {
        putString("state", "complete"); putString("stage", "Prepared output retained")
        putString("destination", retainedPreparedOutput); putString("firmwareId", prep.firmwareId)
        putBoolean("consumed", false)
        putString("timings", PreparationNote.read(this@MainActivity)?.takeIf { it.optString("destination") == retainedPreparedOutput }?.optString("timings") ?: "")
    }
    /**
     * The preparation worker's note of a run LightTouch was closed or restarted during (PreparationNote): show it
     * again if it is still going, add its device if it finished, say why if it failed or Android ended it.
     */
    private fun resumePreparation() {
        val note = PreparationNote.read(this) ?: return
        val job = note.optString("destination")
        note.optString("firmwareId").takeIf { it.isNotEmpty() }?.let { prep.firmwareId = it }
        fun failed(error: String) {
            PreparationNote.clear(this)
            prep.preparation = Bundle().apply { putString("state", "failed"); putString("error", error); putBoolean("consumed", true) }
            preparationOpen = true
        }
        when (note.optString("state")) {
            "preparing" -> {
                val pid = note.optInt("pid")
                val alive = try { File("/proc/$pid/cmdline").readText().contains(":firmware") } catch (_: Exception) { false }
                if (alive) { preparationOpen = true; bindFirmware() }
                else {
                    Thread({ File(job).takeIf { it.isDirectory }?.deleteRecursively() }, "stale-preparation").start()
                    failed("Android stopped the preparation before it finished. Try again.")
                }
            }
            "complete" -> if (job != preparedImported && File(job, "device.lock.json").isFile) {
                retainedPreparedOutput = job; addRetainedWhenReady = true
                prep.preparation = retainedPreparation()
                preparationOpen = true; bindFirmware()
            } else PreparationNote.clear(this)
            "failed" -> failed(note.optString("error").ifEmpty { "Firmware preparation failed" })
            else -> PreparationNote.clear(this)
        }
    }
    private fun preparationActive() = firmwareRestarting || prep.preparation?.getString("state") in listOf("starting", "preparing", "cancelling")
    private fun closePreparation() {
        if (busy || preparationActive()) return
        preparationOpen = false
        // Backed out of the device list: forget the last choice across restarts too.
        if (prep.firmwareId.isEmpty()) getSharedPreferences("preparation-imports", MODE_PRIVATE).edit().remove("firmwareId").apply()
        releaseFirmware()
    }
    /** Ends the preparation worker's process; while a binding remains Android would restart it. */
    private fun releaseFirmware() {
        firmwareExpectedExit = true
        try { firmware?.finish() } catch (e: RemoteException) { error = e.message ?: "Firmware worker unavailable" }
        if (firmwareBound) { unbindService(firmwareConnection); firmwareBound = false }
        firmware = null
    }
    private fun bindFirmware() {
        if (firmwareBound) { unbindService(firmwareConnection); firmwareBound = false }
        firmwareBound = bindService(Intent(this, FirmwareService::class.java), firmwareConnection, BIND_AUTO_CREATE)
    }
    /**
     * A boot ROM already on this phone for [board]'s chip: another device's, or the one earlier builds kept in files/.
     * Each candidate's bytes are checked (64 KB), so the label says which ROM the preparation will use.
     */
    private fun findBootrom(board: String) {
        val rom = Bootrom.forBoard(board) ?: return
        if (prep.romFor == rom && prep.romPath.isNotEmpty() && File(prep.romPath).isFile) return
        prep.romPath = ""; prep.romFor = null; prep.romLabel = ""
        val candidates = records.filter { Bootrom.forBoard(it.getString("board") ?: "") == rom && it.getBoolean("bootromAvailable") }
            .map { File(filesDir, "library/records/${it.getString("id")}/assets/bootrom.bin") to "Reusing the one from ${it.getString("name")}" } +
            listOf(File(filesDir, "bootrom.bin") to "Reusing the saved one")
        Thread({
            val found = candidates.firstOrNull { (file, _) -> file.isFile && file.length() <= (1L shl 20) && rom.matches(file.readBytes()) }
            if (found != null) handler.post {
                if (prep.firmware?.let { Bootrom.forBoard(it.board) } == rom && prep.romFor == null) {
                    prep.romPath = found.first.absolutePath; prep.romFor = rom
                    prep.romLabel = "${found.second} (${rom.label}). It is the same for every iOS version."
                }
            }
        }, "find-bootrom").start()
    }
    @Composable
    private fun PrepareOverlay() {
        val board = prep.firmware?.board
        LaunchedEffect(board, records) { board?.let { findBootrom(it) } }
        PrepareScreen(prep, busy, preparationActive(),
            onDownload = ::downloadIpsw, onChooseIpsw = { stockIpsw.launch(arrayOf("application/octet-stream", "*/*")) },
            onChooseRom = { prepRom.launch(arrayOf("application/octet-stream", "*/*")) }, onDownloadRom = ::downloadBootrom, onPrepare = ::beginPreparation,
            onCancel = { firmware?.cancel(); prep.preparation = prep.preparation?.apply { putString("state", "cancelling") } },
            onRetryImport = { prep.preparation?.let { importPreparedOutput(it) } }, onClose = ::closePreparation)
    }
    private fun beginPreparation() {
        if (preparationActive() || busy) return
        val chosen = prep.firmware ?: return
        if (!prep.ipswReady || !prep.romReady) return
        val worker = firmware ?: run { pendingFirmwareStart = true; firmwareRestarting = true; bindFirmware(); return }
        // Ask the worker, not prep.preparation: Back clears that, and a spent worker would refuse start while its
        // status went on reporting the earlier run (its error and firmware id) as this one's.
        val spent = try { worker.status().getBoolean("consumed") } catch (_: RemoteException) { true }
        if (spent) { restartFirmwareForPreparation(); return }
        val selectedIpsw = prep.ipswPath
        val selectedRom = if (needsBootrom(chosen.board)) prep.romPath else ""
        prep.importError = ""
        prep.preparation = Bundle().apply { putString("state", "starting"); putString("firmwareId", chosen.id) }
        startForegroundService(Intent(this, FirmwareService::class.java))
        Thread({
            try {
                // firmwarekit bakes the APK's own guest helpers and packages into the system.
                val tools = GuestTools.firmwareTools(this)
                val state = worker.start(chosen.id, selectedIpsw, tools.absolutePath, selectedRom)
                handler.post { prep.preparation = state }
            } catch (e: Exception) { handler.post { prep.preparation = Bundle().apply { putString("state", "failed"); putString("error", e.message ?: "Cannot start preparation"); putBoolean("consumed", true) } } }
        }, "prepare-start").start()
    }
    private fun restartFirmwareForPreparation() {
        if (firmwareRestarting) return
        val old = firmware ?: run { pendingFirmwareStart = true; firmwareRestarting = true; bindFirmware(); return }
        val oldBinder = old.asBinder()
        firmwareRestarting = true
        firmwareExpectedExit = true
        pendingFirmwareStart = true
        if (firmwareBound) { unbindService(firmwareConnection); firmwareBound = false }
        Thread({
            val died = CountDownLatch(1)
            val recipient = IBinder.DeathRecipient { died.countDown() }
            try {
                if (oldBinder.isBinderAlive) {
                    try { oldBinder.linkToDeath(recipient, 0) } catch (_: DeadObjectException) { died.countDown() }
                    try { old.finish() } catch (_: DeadObjectException) { died.countDown() }
                    check(!oldBinder.isBinderAlive || died.await(5, TimeUnit.SECONDS)) { "Previous firmware worker has not exited. Retry after it stops." }
                }
                handler.post { firmware = null; prep.preparation = null; bindFirmware() }
            } catch (e: Exception) { handler.post { pendingFirmwareStart = false; firmwareRestarting = false; error = e.message ?: "Firmware worker restart failed" } }
            finally { try { oldBinder.unlinkToDeath(recipient, 0) } catch (_: java.util.NoSuchElementException) { } }
        }, "firmware-worker-restart").start()
    }
    private fun pollPreparation(state: Bundle) {
        if (firmwareRestarting) return
        val phase = state.getString("state")
        if (phase == "idle" && retainedPreparedOutput.isNotEmpty()) return
        if (phase == "idle" && prep.preparation?.getString("state") != "starting") { prep.preparation = null; return }
        // The user went back from a finished run; the worker keeps reporting it until it is replaced.
        if (phase in listOf("failed", "cancelled") && prep.preparation == null) return
        // Shown here, so not again at the next launch.
        if (phase in listOf("failed", "cancelled")) PreparationNote.clear(this)
        prep.preparation = state
        if (phase == "complete" && state.getString("destination") != preparedImported) {
            retainedPreparedOutput = state.getString("destination") ?: ""
            getSharedPreferences("preparation-imports", MODE_PRIVATE).edit().putString("pendingPrepared", retainedPreparedOutput).apply()
        }
        state.getString("firmwareId")?.takeIf { it.isNotEmpty() }?.let { prep.firmwareId = it }
        // After the catalog (it names the device); the next poll tries again.
        if (!preparedImportInFlight && phase == "complete" && prep.catalog.isNotEmpty() && state.getString("destination") != preparedImported) importPreparedOutput(state)
    }
    private fun importPreparedOutput(state: Bundle) {
        if (preparedImportInFlight) return
        val worker = engine
        val destination = requireNotNull(state.getString("destination")); preparedImported = destination; busy = true; preparedImportInFlight = true
        prep.importPhase = ""; prep.importProgress = -1f
        val chosen = prep.firmware; val ipsw = prep.ipswPath
        // Only the boot ROM this device starts from: one picked for another device stays out of its record.
        val rom = if (prep.bootrom != null && prep.romReady) prep.romPath else ""
        val board = chosen?.board ?: prep.firmwareId.substringBefore('-'); val build = chosen?.build ?: prep.firmwareId.substringAfter('-')
        val before = records.mapNotNull { it.getString("id") }.toSet()
        val timings = state.getString("timings") ?: ""; val started = SystemClock.elapsedRealtime()
        Thread({
            var name = ""; var selected = false
            val result = try {
                checkNotNull(worker) { "Device worker unavailable" }
                name = defaultDeviceName(board, build, firmwareTargets, devices.map { it.name })
                val stopWatching = watchImport(worker) { text, fraction -> prep.importPhase = text; prep.importProgress = fraction }
                // This app's own firmwarekit output moves in as it is; a packed folder from elsewhere is copied.
                val own = destination.startsWith(filesDir.absolutePath) && File(destination, "device.lock.json").isFile
                try { if (own) worker.adoptPrepared(destination, name, rom) else worker.importPrepared(destination, name, rom) }
                finally { stopWatching() }.also { result ->
                    if (result.isEmpty()) {
                        val created = worker.records().firstOrNull { it.getString("id") !in before }
                        selected = created != null && status !in listOf("running", "paused", "starting", "stopping") && worker.select(created.getString("id")).isEmpty()
                    }
                }
            } catch (e: Exception) { e.message ?: "Device worker exited during import" }
            if (result.isEmpty()) {
                // The device holds its own copy now; the prepared output and the IPSW only take space.
                File(destination).deleteRecursively(); File("$destination.native.log").delete()
                if (ipsw.startsWith(filesDir.absolutePath)) File(ipsw).delete()
            }
            handler.post {
                busy = false; preparedImportInFlight = false; prep.importError = result
                if (result.isEmpty()) {
                    retainedPreparedOutput = ""
                    if (PreparationNote.read(this)?.optString("destination") == destination) PreparationNote.clear(this)
                    if (ipsw.startsWith(filesDir.absolutePath)) prep.ipswPath = ""
                    getSharedPreferences("preparation-imports", MODE_PRIVATE).edit().putString("preparedImported", destination).remove("pendingPrepared").remove("ipsw").apply()
                    refreshDevices(); releaseFirmware(); preparationOpen = false; prep.preparation = null; prep.step = PrepareStep.DEVICE
                    if (selected) {
                        setup.reset(); setup.created = name; setup.step = SetupStep.DONE; overlay = Screen.SETUP
                        setup.timings = timings + "Adding the device\t${(SystemClock.elapsedRealtime() - started) / 100 / 10.0}\n"
                    }
                    else notice = "Added $name; stop the running device to switch to it"
                }
            }
        }, "prepared-device-import").start()
    }
    private fun rememberIpsw(path: String, firmwareId: String) {
        prep.ipswPath = path; prep.ipswFor = firmwareId; prep.ipswError = ""
        getSharedPreferences("preparation-imports", MODE_PRIVATE).edit().putString("ipsw", path).putString("ipswFor", firmwareId).putString("firmwareId", firmwareId).apply()
    }
    private fun downloadIpsw() {
        val chosen = prep.firmware ?: return
        prep.ipswError = ""
        IpswDownload.start(this, chosen) { file, _ -> handler.post { if (file != null) rememberIpsw(file.absolutePath, chosen.id) } }
    }
    /** Copies a chosen IPSW in, checking its SHA-1 against the chosen version's on the way. */
    private fun importStockIpsw(uri: Uri) {
        val chosen = prep.firmware ?: return
        if (preparationActive() || busy) return
        busy = true; prep.ipswError = ""; prep.ipswChecking = "Copying ${displayName(uri)}"
        DownloadService.hold(applicationContext, "Copying iOS ${chosen.label}")
        Thread({
            val target = File(IpswDownload.directory(this).apply { mkdirs() }, chosen.fileName)
            val partial = File(target.path + ".copy")
            try {
                val digest = java.security.MessageDigest.getInstance("SHA-1")
                val total = DocumentFile.fromSingleUri(this, uri)?.length() ?: 0L
                require(chosen.bytes <= 0 || total <= 0 || total == chosen.bytes) { "That file is not the iOS ${chosen.label} IPSW for the ${shortProductName(chosen.productType)}" }
                contentResolver.openInputStream(uri)?.use { input -> partial.outputStream().use { output ->
                    val buffer = ByteArray(1 shl 20); var done = 0L
                    while (true) {
                        val n = input.read(buffer); if (n < 0) break
                        output.write(buffer, 0, n); digest.update(buffer, 0, n); done += n
                        if (total > 0) handler.post { prep.ipswChecking = "Copying and checking… ${done * 100 / total}%" }
                    }
                } } ?: error("Cannot read the IPSW")
                val sha1 = digest.digest().joinToString("") { "%02x".format(it) }
                require(chosen.sha1.isEmpty() || sha1 == chosen.sha1) { "That file is not the iOS ${chosen.label} IPSW for the ${shortProductName(chosen.productType)}" }
                check(partial.renameTo(target)) { "Could not save the IPSW" }
                handler.post { rememberIpsw(target.absolutePath, chosen.id); prep.ipswChecking = ""; busy = false }
            } catch (e: Exception) {
                partial.delete()
                handler.post { prep.ipswError = e.message ?: "Could not use that IPSW"; prep.ipswChecking = ""; busy = false }
            } finally { DownloadService.release(applicationContext) }
        }, "stock-ipsw-import").start()
    }
    private fun downloadBootrom() {
        val rom = prep.bootrom ?: return
        prep.romChecking = true; prep.romError = ""
        Thread({
            try {
                val target = File(filesDir, "prepare-bootrom-${rom.name.lowercase()}.bin")
                rom.fetch(target)
                handler.post { prep.romPath = target.absolutePath; prep.romFor = rom; prep.romLabel = "Downloaded · ${rom.label}"; prep.romChecking = false }
            } catch (e: Exception) { handler.post { prep.romError = "Could not download the boot file: ${e.message}"; prep.romChecking = false } }
        }, "bootrom-download").start()
    }
    private fun importPrepRom(uri: Uri) {
        val rom = prep.bootrom ?: return
        Thread({
            try {
                val target = File(filesDir, "prepare-bootrom-${rom.name.lowercase()}.bin")
                contentResolver.openInputStream(uri)?.use { input -> target.outputStream().use { input.copyTo(it) } } ?: error("Cannot read the boot file")
                require(target.length() in 1..(1L shl 20)) { "That file is not a boot ROM" }
                // The chip's mask ROM: every good dump is these bytes, and the device boots it.
                require(rom.matches(target.readBytes())) { "That file is not the ${rom.label} boot ROM" }
                val name = displayName(uri)
                handler.post { prep.romPath = target.absolutePath; prep.romFor = rom; prep.romLabel = "$name · ${rom.label}" }
            } catch (e: Exception) { handler.post { prep.romError = e.message ?: "Could not use that boot file" } }
        }, "prepare-bootrom").start()
    }
    private fun refreshDevices(afterDelete: Boolean = false) {
        Thread({
            try {
                var loaded = engine?.records()?.toList() ?: return@Thread
                // Earlier builds named devices after their firmware id ("n72ap-7E18 prepared"); give those the friendly default once.
                if (firmwareTargets.isNotEmpty()) {
                    val names = loaded.mapNotNull { it.getString("name") }.toMutableList()
                    val renamed = loaded.count { record ->
                        val name = record.getString("name") ?: ""; val board = record.getString("board") ?: ""; val build = record.getString("build") ?: ""
                        if (!isGeneratedDeviceName(name, board, build)) return@count false
                        val friendly = defaultDeviceName(board, build, firmwareTargets, names - name)
                        friendly != name && engine?.rename(record.getString("id"), friendly).isNullOrEmpty().also { if (it) { names -= name; names += friendly } }
                    }
                    if (renamed > 0) loaded = engine?.records()?.toList() ?: loaded
                }
                handler.post {
                    records = loaded; recordsLoaded = true
                    if (afterDelete && loaded.isEmpty()) { setup.reset(); overlay = Screen.SETUP }
                }
            } catch (e: RemoteException) { handler.post { error = "Device library unavailable: ${e.message}" } }
        }, "device-library").start()
    }
    private fun bindWorker() {
        workerEvidenceDelivery.advance()
        if (bound) { unbindService(connection); bound = false }
        // BIND_IMPORTANT gives the worker this activity's scheduling group while
        // it is on screen (top-app: every core, fast frequency ramp). Without it
        // the worker sits in foreground, which on Pixel excludes the prime core
        // and leaves the vCPU's cores near minimum frequency.
        bound = bindService(Intent(this, EngineService::class.java), connection, BIND_AUTO_CREATE or BIND_IMPORTANT)
    }
    private fun workerExited() {
        val evidenceGeneration = workerEvidenceDelivery.retire(engine != null || workerPid != 0) ?: return
        val unexpected = !engineExpectedExit && (guestStartRequested || sessionOwned || status in listOf("running", "starting", "paused"))
        val exitedPid = workerPid
        profileRecording = false; profileBusy = false
        engine = null; workerPid = 0; sessionOwned = false; displayAsleep = false; stopSensors(); displayView?.keepScreenOn = false
        status = if (unexpected) "failed" else "stopped"
        detail = ""
        if (unexpected) error = "Guest worker exited unexpectedly; writable state retained. Start creates a fresh worker. Private diagnostics are retained."
        Thread({
            WorkerExitEvidence.capture(applicationContext, exitedPid)
            val terminal = try {
                val file = File(filesDir, "worker-shutdown-$exitedPid.json")
                file.takeIf { it.exists() }?.readText()?.let { JSONObject(it) }?.takeIf { it.optInt("workerPid") == exitedPid }
            } catch (_: Exception) { null }
            val retained = try { File(filesDir, "worker-stop-error-$exitedPid.txt").takeIf { it.exists() }?.readText() } catch (_: java.io.IOException) { null }
            if (terminal != null) {
                val reported = terminal.optString("error")
                if (reported.isNotEmpty()) handler.post { workerEvidenceDelivery.deliver(evidenceGeneration) { error = reported } }
                else if (!unexpected && terminal.optString("reason") == "native-terminal" && terminal.optString("nativeTerminalState") == "stopped") {
                    handler.post { workerEvidenceDelivery.deliver(evidenceGeneration) { error = "" } } // Own current terminal report wins over an earlier error.
                }
            } else retained?.takeIf { it.isNotEmpty() }?.let { handler.post { workerEvidenceDelivery.deliver(evidenceGeneration) { error = it } } }
        }, "worker-retained-error").start()
    }
    private fun startGuest(coldRecovery: Boolean = false, retainedRequest: GuestStartRequest? = null) {
        if (engineRestarting) return
        val request = retainedRequest ?: GuestStartRequest(
            selected?.id ?: run { error = "Select a device"; status = "stopped"; return }, coldRecovery)
        // Preserve the exact record and explicit recovery choice across worker
        // disposal/rebinding; later device-list refreshes cannot change the request.
        pendingGuestRequest = request
        val worker = engine
        if (worker == null) { pendingGuestStart = true; status = "starting"; bindWorker(); return }
        if (sessionOwned) { restartGuestForStart(worker); return }
        workerEvidenceDelivery.advance()
        engineExpectedExit = false; guestStartRequested = true; status = "starting"
        startForegroundService(Intent(this, EngineService::class.java))
        Thread({
            try {
                // Newer guest tools in this build of LightTouch reach existing devices here; a failure leaves the old ones.
                try { GuestTools.newerFor(this, request.recordId)?.let { tools ->
                    val result = worker.setGuestPackage(request.recordId, tools.absolutePath)
                    android.util.Log.i("LightTouch", if (result.isEmpty()) "Guest tools updated for ${request.recordId}" else "Guest tools not updated: $result")
                } } catch (failure: Exception) { android.util.Log.w("LightTouch", "Guest tools not updated", failure) }
                val b = if (request.coldRecovery) worker.recoverStart(request.recordId) else worker.start(request.recordId)
                handler.post { if (engine?.asBinder() == worker.asBinder()) error = b.getString("error") ?: "" }
            }
            catch (failure: Exception) { handler.post {
                if (engine?.asBinder() == worker.asBinder()) {
                    error = "Guest start failed: ${failure.message ?: "worker exited"}. Writable state and private diagnostics retained."
                    status = "failed"
                }
            } }
        }, "guest-start").start()
    }
    private fun stopGuest() {
        val worker = engine ?: return
        engineExpectedExit = true; status = "stopping"; stopSensors()
        Thread({
            try { worker.stop() }
            catch (failure: RemoteException) { handler.post {
                if (engine?.asBinder() == worker.asBinder()) error = "Stop admission failed; writable state retained: ${failure.message ?: "worker exited"}"
            } }
        }, "guest-stop-request").start()
    }
    private fun restartGuestForStart(old: IEngine) {
        workerEvidenceDelivery.advance()
        engineRestarting = true; engineExpectedExit = true; pendingGuestStart = true; status = "stopping"
        if (bound) { unbindService(connection); bound = false }
        val binder = old.asBinder()
        Thread({
            val died = CountDownLatch(1); val recipient = IBinder.DeathRecipient { died.countDown() }
            try {
                if (binder.isBinderAlive) {
                    try { binder.linkToDeath(recipient, 0) } catch (_: DeadObjectException) { died.countDown() }
                    try { old.stop() } catch (_: DeadObjectException) { died.countDown() }
                    check(!binder.isBinderAlive || died.await(45, TimeUnit.SECONDS)) { "Previous guest worker has not exited. Retry after it stops." }
                }
                handler.post { engine = null; sessionOwned = false; engineRestarting = false; bindWorker() }
            } catch (failure: Exception) { handler.post { engineRestarting = false; pendingGuestStart = false; error = failure.message ?: "Guest worker restart failed"; status = "failed" } }
            finally { try { binder.unlinkToDeath(recipient, 0) } catch (_: java.util.NoSuchElementException) { } }
        }, "guest-worker-restart").start()
    }
    override fun pulse(id: Int) = pulse(id, 100)
    private fun pulse(button: Int, holdMs: Long) {
        val worker = engine ?: return
        buttonCalls.execute {
            try { worker.button(button, true); Thread.sleep(holdMs); worker.button(button, false) }
            catch (_: RemoteException) { handler.post { error = "Guest worker unavailable; Start a fresh worker." } }
        }
    }
    private fun capture() {
        val path = File(filesDir, "screenshots/guest-${System.currentTimeMillis()}.png").absolutePath
        Thread({
            try {
                val result = engine?.screenshot(path) ?: "Worker unavailable"
                require(result.isEmpty()) { result }
                val values = ContentValues().apply {
                    put(MediaStore.Images.Media.DISPLAY_NAME, File(path).name)
                    put(MediaStore.Images.Media.MIME_TYPE, "image/png")
                    put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/LightTouch")
                    put(MediaStore.Images.Media.IS_PENDING, 1)
                }
                val uri = contentResolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values) ?: error("Cannot create screenshot")
                try {
                    contentResolver.openOutputStream(uri)?.use { output -> File(path).inputStream().use { it.copyTo(output) } } ?: error("Cannot write screenshot")
                    values.clear(); values.put(MediaStore.Images.Media.IS_PENDING, 0)
                    contentResolver.update(uri, values, null, null)
                } catch (e: Exception) { contentResolver.delete(uri, null, null); throw e }
                handler.post { notice = "Screenshot saved to Pictures/LightTouch" }
            } catch (e: Exception) { handler.post { error = e.message ?: "Screenshot failed" } }
        }, "screenshot").start()
    }
    private fun copyAsset(uri: Uri, targetName: String) {
        busy = true
        Thread({
            try {
                val target = File(filesDir, targetName)
                val temporary = File(filesDir, "$targetName.partial")
                contentResolver.openInputStream(uri)?.use { input -> temporary.outputStream().use { input.copyTo(it) } } ?: error("Cannot read file")
                check(temporary.renameTo(target)) { "Could not publish imported file" }
                // Guest tools also replace the selected device's package; the guest
                // installs the newer serial at its next boot.
                val device = if (targetName == "guest.itpack") selected?.id else null
                if (device != null) {
                    val result = engine?.setGuestPackage(device, target.absolutePath) ?: error("Worker unavailable")
                    require(result.isEmpty()) { result }
                }
                handler.post { notice = if (device != null) "Guest tools updated; they install at the next start" else "Imported $targetName"; busy = false }
            } catch (e: Exception) { handler.post { error = e.message ?: "Import failed"; busy = false } }
        }, "asset-import").start()
    }
    private var listening = false
    // Sensor callbacks arrive here, not on the UI thread; motion() is a oneway Binder call.
    private val sensorThread by lazy { HandlerThread("guest-motion").apply { start() } }
    @Volatile private var motionWorker: IEngine? = null
    @Volatile private var displayRotation = 0
    /** Only the sensors the guest board models are opened; the 2G has an accelerometer and no gyroscope. */
    private fun startSensors() {
        val record = records.firstOrNull { it.getBoolean("selected") }
        if (!settings.tilt || listening || record == null) return
        motionWorker = engine
        displayRotation = (display?.rotation ?: 0) * 90
        val thread = Handler(sensorThread.looper)
        // 100 Hz: the runtime admits up to 200 Hz per stream. Units are already m/s² and rad/s;
        // axis and rotation mapping into guest portrait axes is native (lt_map_motion).
        if (record.getBoolean("acceleration")) sensors.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)?.let { sensors.registerListener(this, it, 10_000, thread) }
        if (record.getBoolean("gyro")) sensors.getDefaultSensor(Sensor.TYPE_GYROSCOPE)?.let { sensors.registerListener(this, it, 10_000, thread) }
        listening = true
    }
    private fun stopSensors() { sensors.unregisterListener(this); listening = false; motionWorker = null }
    override fun onSensorChanged(event: SensorEvent) {
        val kind = if (event.sensor.type == Sensor.TYPE_GYROSCOPE) 2 else 1
        try { motionWorker?.motion(kind, event.timestamp, event.values.copyOf(3), displayRotation) } catch (_: RemoteException) { }
    }
    private fun sendButton(id: Int, down: Boolean) {
        val worker = engine ?: return
        buttonCalls.execute { try { worker.button(id, down) } catch (_: RemoteException) { } }
    }
    /** Full screen hides the system bars; a swipe from the edge shows them briefly. */
    private fun applyWindowMode(immersive: Boolean) {
        if (Build.VERSION.SDK_INT >= 30) window.insetsController?.let { controller ->
            if (immersive) { controller.hide(PlatformInsets.Type.systemBars()); controller.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE }
            else controller.show(PlatformInsets.Type.systemBars())
        } else @Suppress("DEPRECATION") {
            window.decorView.systemUiVisibility = if (immersive) (android.view.View.SYSTEM_UI_FLAG_FULLSCREEN or android.view.View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
                android.view.View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or android.view.View.SYSTEM_UI_FLAG_LAYOUT_STABLE or android.view.View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or
                android.view.View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN) else (android.view.View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                android.view.View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or android.view.View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN)
        }
    }
    private fun recordTarget(): StoreTarget? = records.firstOrNull { it.getBoolean("selected") }?.let { firmwareTargets["${it.getString("board")}-${it.getString("build")}"] }
    private fun selectedRecordId() = records.firstOrNull { it.getBoolean("selected") }?.getString("id") ?: ""
    private fun openStore() {
        overlay = Screen.STORE
        store.close() // Reopen on the search, not an app's details.
        store.useRecord(selectedRecordId(), recordTarget())
        if (muxSocket.isNotEmpty()) store.refreshGuest()
    }
    override fun onAccuracyChanged(sensor: Sensor, accuracy: Int) = Unit
    override fun onStart() { super.onStart(); active = true; installer.visible = true; DownloadService.clearFinished(this); DisplayOwners.active.started(this); displayView?.keepScreenOn = status in listOf("starting", "running", "stopping"); handler.post(poll) }
    override fun onResume() {
        super.onResume(); displayResumed = true
        displayOwnerTicket = DisplayOwners.active.resumed(this, hasWindowFocus())
        displayView?.attachActiveSurface()
    }
    override fun onPostResume() {
        super.onPostResume()
        displayOwnerTicket = DisplayOwners.active.focusChanged(this, hasWindowFocus())
        displayView?.attachActiveSurface()
        // Coming back to a guest LightTouch paused on the way out: carry on, once the display is reattached.
        if (pausedByBackground && settings.autoResume) handler.postDelayed({
            if (pausedByBackground && active && displayResumed && status == "paused") setPaused(false)
            pausedByBackground = false
        }, 300)
    }
    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) applyWindowMode(base == Screen.FULLSCREEN && overlay == null) // Transient bars shown by a swipe hide again.
        // Consult actual Activity focus too: an old callback alone is not authority.
        displayOwnerTicket = DisplayOwners.active.focusChanged(this, hasFocus && hasWindowFocus())
        displayView?.attachActiveSurface()
    }
    override fun onPause() {
        displayResumed = false; DisplayOwners.active.paused(this)
        displayView?.cancelOwnedGesture(); super.onPause()
    }
    override fun onStop() {
        val wasOwner = ownsDisplay()
        active = false; displayView?.keepScreenOn = false; stopSensors()
        // Owner validation also occurs inside the native guard; a handoff after
        // this UI check still cannot make old A pause/cancel/detach new B.
        installer.visible = false
        if (wasOwner && ((sessionOwned && status !in listOf("stopping", "stopped", "failed")) || status in listOf("running", "starting"))) {
            // Installing apps needs the guest running: it lets go of the display, and keeps going until the queue is empty.
            if (installer.queue.active) displayView?.retireDisplay()
            else {
                pausedByBackground = status == "running" && overlay != Screen.HITCH
                displayView?.backgroundOwnedDisplay()
            }
        }
        else displayView?.retireDisplay()
        DisplayOwners.active.stopped(this, displayOwnerTicket)
        handler.removeCallbacks(poll); monitor.setActive(false, false); super.onStop()
    }
    /** The install queue emptied. With LightTouch off screen the guest pauses now, as it would have on leaving, and resumes on return. */
    private fun installsFinished() {
        if (active || status != "running" || !sessionOwned || overlay == Screen.HITCH) return
        val worker = engine ?: return
        pausedByBackground = true
        Thread({ try { worker.pause(true) } catch (_: RemoteException) { } }, "install-idle-pause").start()
    }
    override fun onDestroy() { if (installer.onIdle === idleHook) installer.onIdle = null; buttonCalls.shutdown(); monitor.close(); displayView?.retireDisplay(); DisplayOwners.active.stopped(this, displayOwnerTicket); workerEvidenceDelivery.advance(); handler.removeCallbacksAndMessages(null); stopSensors(); if (bound) unbindService(connection); if (firmwareBound) unbindService(firmwareConnection); super.onDestroy() }
}
