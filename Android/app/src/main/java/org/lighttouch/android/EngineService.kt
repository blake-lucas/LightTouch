package org.lighttouch.android

import android.app.*
import android.content.Intent
import android.media.*
import android.os.*
import android.view.Surface
import java.io.File
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicBoolean

/** Same app UID, separate native address space. Every session consumes this process. */
class EngineService : Service() {
    @Volatile private var error = ""
    @Volatile private var userPaused = false
    @Volatile private var focusPaused = false
    /** The running device's name, for the notification; set by start. */
    @Volatile private var deviceName = ""
    /** startForeground has run in this worker, so the notification may be updated. */
    @Volatile private var foregrounded = false
    private val started = WorkerStartAdmission()
    private val failureRecorded = AtomicBoolean(false)
    private val stopping = AtomicBoolean(false)
    private val alive = AtomicBoolean(true)
    @Volatile private var audio: AudioTrack? = null
    /** The guest's audio plays (mixed with other apps' audio: LightTouch takes no audio focus). */
    @Volatile private var audioOn = false
    private val touchAdmissionGuard = Any()
    private val lifecycleAdmission = WorkerLifecycleAdmission()
    private val touchTimestampOrder = TouchTimestampOrder()
    private val displayCancellations = DisplayCancellationHistory()
    private val audioGuard = AudioPacketEpoch()
    private val audioObservations = AudioObservations()
    @Volatile private var audioError = ""
    private val buttonDownAdmissions = java.util.concurrent.atomic.AtomicLongArray(2)
    private val buttonUpAdmissions = java.util.concurrent.atomic.AtomicLongArray(2)
    private val buttonRejections = java.util.concurrent.atomic.AtomicLong(0)
    private val shutdownWatchdog = ShutdownWatchdog(BuildConfig.SHUTDOWN_GRACE_MS, SystemClock::elapsedRealtime,
        requestNativeStop = { Native.stop() }, cancelNativeContacts = { Native.cancelInput() },
        nativeStatus = { Native.status() }, dispose = { receipt -> disposeAfterShutdown(receipt) },
        persistTerminal = { receipt -> persistTerminalReceipt(receipt) })
    private val profileCapture by lazy { ProfileCapture(applicationContext, ::observations) }
    private val performanceSampler by lazy { WorkerPerformanceSampler(applicationContext) }
    private val usb by lazy { UsbBridge(applicationContext) }
    // The daemon's parent-death signal follows the thread that forked it, so fork from one that never exits.
    private val usbThread = java.util.concurrent.Executors.newSingleThreadExecutor { Thread(it, "usb-bridge") }
    @Volatile private var usbError = ""
    private val networkSession = NetworkSession()
    @Volatile private var networkSetup = "Off"
    /** The guest setup ran (or is running) this session; it keeps its certificate and proxy when networking is
     * turned off and on again. */
    private val guestNetworkStarted = java.util.concurrent.atomic.AtomicBoolean(false)
    @Volatile private var networkSetupError = false
    private val networkThread = java.util.concurrent.Executors.newSingleThreadExecutor { Thread(it, "network-setup") }
    private val cpuSampler by lazy { WorkerCpuSampler() }
    private val profileTail = ProfileTail()
    @Volatile private var detailedTiming = false
    // Why detailed timing is not recording (e.g. LT_PROFILE=off); retried only when it is turned on again.
    @Volatile private var detailedTimingError = ""
    // Detailed timing keeps an automatic capture running while the guest does; this thread
    // starts one once the guest runs and again after a capture reaches its limit.
    private val detailedTimingThread = java.util.concurrent.Executors.newSingleThreadScheduledExecutor { Thread(it, "detailed-timing") }
    private val binder = object : IEngine.Stub() {
        override fun rename(recordId: String, name: String): String = Native.rename(File(filesDir, "library").absolutePath, recordId, name)
        override fun remove(recordId: String): String = Native.remove(File(filesDir, "library").absolutePath, recordId)
        override fun importProgress(): Bundle = Native.importProgress().let { p ->
            Bundle().apply { putString("phase", p[0]); putLong("done", p[1].toLongOrNull() ?: 0); putLong("total", p[2].toLongOrNull() ?: 0) }
        }
        override fun frameStats(sinceSequence: Long): Bundle = Bundle().apply {
            putLongArray("frames", Native.frameTimeline(sinceSequence))
            putLong("nowNs", System.nanoTime())
            putLongArray("cpu", cpuSampler.sample())
            profileTail.follow(if (detailedTiming && profileCapture.automatic) profileCapture.currentEvents() else null)
            profileTail.summary()?.let { putBundle("detailed", it) }
            if (detailedTiming && detailedTimingError.isNotEmpty()) putString("detailedError", detailedTimingError)
        }
        override fun setPresentScale(mode: Int) = Native.setPresentScale(mode)
        override fun setPresentVisible(visible: Boolean) = Native.setPresentVisible(visible)
        override fun setDetailedTiming(enabled: Boolean) {
            detailedTiming = enabled; detailedTimingError = ""
            if (!enabled && profileCapture.automatic) profileCapture.finish("detailed-timing-off")
        }
        override fun applyNetworkSettings() {
            synchronized(networkSession) {
                if (!started.isConsumed() || stopping.get()) return
                networkSession.request(NetworkConfiguration.load(applicationContext))
                refreshNetworkSettings()
            }
        }
        /** Freeze what led up to now for the hitch inspector: the frame timeline, and the detailed
         * timing capture (closed and kept) when one is running. The caller has paused the guest. */
        override fun hitchSnapshot(): Bundle {
            val events = if (profileCapture.automatic) profileCapture.currentEvents() else null
            val directory = events?.parentFile ?: File(filesDir, "hitches/${System.currentTimeMillis()}").apply { mkdirs() }
            if (events != null) profileCapture.finish("hitch", keep = true)
            val frames = Native.frameTimeline(0)
            File(directory, "frames.tsv").bufferedWriter().use { out ->
                out.write("# sequence published-ns presented-ns presenter-work-ns (CLOCK_MONOTONIC)\n")
                for (i in frames.indices step 4) out.write("${frames[i]} ${frames[i + 1]} ${frames[i + 2]} ${frames[i + 3]}\n")
            }
            return Bundle().apply {
                putString("directory", directory.absolutePath); putBoolean("detailed", events != null)
                putLong("snapshotNs", System.nanoTime()); putString("engineLog", Native.status().getOrElse(3) { "" })
            }
        }
        // Asked of the guest tools, at most ~2.5 s.
        override fun frontmostApp(): Bundle = Bundle().apply {
            val app = profileCapture.frontmostIdentity()
            putString("bundleId", app?.first ?: ""); putString("appName", app?.second ?: "")
        }
        override fun performance(observationEpoch: Long): Bundle = performanceSampler.request(Binder.getCallingPid(), observationEpoch)
        private fun requireDebugDiagnosis() { check(applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0) { "Debug diagnosis is unavailable in this build" } }
        override fun debugShutdownProgress(): Bundle {
            requireDebugDiagnosis()
            // Do not route this through status()/observations(): those enter JNI.
            return shutdownProgressBundle(shutdownWatchdog.cachedProgress())
        }
        override fun debugArmQmp(): String { requireDebugDiagnosis(); return Native.debugArmQmp() }
        override fun debugArmVcpuAccounting(): String {
            requireDebugDiagnosis()
            if (stopping.get()) return "This worker is disposing; vCPU accounting requires a fresh worker"
            return started.configureUnused { Native.debugArmVcpuAccounting() }
        }
        override fun debugSelectTcgIrCache(enabled: Boolean): String {
            requireDebugDiagnosis()
            if (stopping.get()) return "This worker is disposing; experimental cache selection requires a fresh worker"
            return started.configureUnused("Experimental cache selection") { Native.debugSelectTcgIrCache(enabled) }
        }
        override fun debugEnableTouchTrace(): String {
            requireDebugDiagnosis()
            if (started.isConsumed()) return "Touch trace selection requires a fresh unused worker"
            return Native.debugEnableTouchTrace()
        }
        override fun debugSelectGraphicsProvider(choice: Int): String {
            requireDebugDiagnosis()
            if (started.isConsumed()) return "Graphics provider selection requires a fresh unused worker"
            return Native.debugSelectGraphicsProvider(choice)
        }
        override fun debugQmpSnapshot(): Bundle {
            requireDebugDiagnosis()
            val result = Native.debugQmpSnapshot()
            return Bundle().apply { putString("error", result[0]); putString("path", result[1]) }
        }
        override fun importPrepared(source: String, displayName: String, rom: String): String = Native.importBase(source, File(filesDir, "library").absolutePath, displayName, rom, "")
        override fun adoptPrepared(source: String, displayName: String, rom: String): String = Native.adoptPrepared(source, File(filesDir, "library").absolutePath, displayName, rom)
        override fun setGuestPackage(recordId: String, path: String): String = Native.setGuestPackage(File(filesDir, "library").absolutePath, recordId, path)
        override fun records(): Array<Bundle> = Native.records(File(filesDir, "library").absolutePath).mapNotNull { row ->
            if (row[0].isEmpty()) { error = row.getOrElse(1) { "Device library unavailable" }; null }
            else Bundle().apply {
                putString("id", row[0]); putString("name", row[1]); putString("board", row[2]); putString("build", row[3])
                putBoolean("selected", row[4] == "1"); putBoolean("dirty", row[5] == "1")
                putBoolean("bootromAvailable", row.getOrElse(9) { "0" } == "1"); putBoolean("guestPackageAvailable", row.getOrElse(10) { "0" } == "1")
                putString("contacts", row[6]); putBoolean("gyro", row[7] == "1"); putBoolean("acceleration", row[8] == "1")
            }
        }.toTypedArray()
        override fun select(recordId: String): String = Native.select(File(filesDir, "library").absolutePath, recordId)
        override fun start(recordId: String): Bundle = startRecord(recordId, coldRecovery = false)
        override fun recoverStart(recordId: String): Bundle = startRecord(recordId, coldRecovery = true)
        private fun startRecord(recordId: String, coldRecovery: Boolean): Bundle {
            if (stopping.get()) return Bundle().apply {
                putString("state", "stopping"); putString("error", "This worker is disposing; recovery and Start require a fresh worker.")
            }
            if (!started.tryConsume()) return status().apply {
                putString("error", "Start and recovery require a fresh unused guest worker. Stop this worker first.")
            }
            val libraryRoot = File(filesDir, "library").absolutePath
            deviceName = Native.records(libraryRoot).firstOrNull { it[0] == recordId }?.getOrNull(1) ?: ""
            foreground()
            // Before the engine starts: its USB controller dials the bridge when the guest brings the port up.
            usbError = usbThread.submit<String?> { usb.start(recordId) }.get() ?: ""
            // Perf work on debug builds (create these with run-as): files/debug-vcpu-accounting logs the
            // vCPU's run/halt/BQL split every 5 s to the record's state/engine-PID.log; files/debug-env
            // holds KEY=VALUE engine environment lines, e.g. QEMU_PERFMAP_DIR=<files>/perfmap for JIT
            // symbols or IT_GLES_PROF=1 (see docs/PERFORMANCE.md).
            if (applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0) {
                if (File(filesDir, "debug-vcpu-accounting").exists()) Native.debugArmVcpuAccounting()
                File(filesDir, "debug-env").takeIf { it.exists() }?.readLines()?.forEach { line ->
                    val (key, value) = line.split('=', limit = 2).takeIf { it.size == 2 } ?: return@forEach
                    android.system.Os.setenv(key.trim(), value.trim(), true)
                }
            }
            // The Settings switch (AppSettings.headphones); the engine applies it only on boards that model the jack.
            val headphones = getSharedPreferences("ui", MODE_PRIVATE).getBoolean("headphones", true)
            synchronized(networkSession) {
                val network = NetworkConfiguration.load(applicationContext)
                networkSession.begin(network)
                guestNetworkStarted.set(false)
                networkSetup = if (network.enabled) "Waiting for guest tools…" else "Off"
                val roots = listOf("/apex/com.android.conscrypt/cacerts", "/system/etc/security/cacerts").firstOrNull { File(it).isDirectory } ?: "/system/etc/security/cacerts"
                error = Native.networkPrepare(filesDir.absolutePath, recordId, network.nativeMode, network.archiveDate, network.localNetwork, roots)
                if (error.isEmpty()) error = if (coldRecovery) Native.recoverStart(libraryRoot, recordId, headphones) else Native.start(libraryRoot, recordId, headphones)
                if (error.isEmpty() && network.enabled) startGuestNetwork()
            }
            if (error.isEmpty()) { File(filesDir, "worker-stop-error.txt").delete(); if (!userPaused && !stopping.get()) startAudioOutput() }
            val response = status()
            if (error.isNotEmpty()) Handler(mainLooper).postDelayed({
                persistStopError(error)
                persistObservations("start-failed")
                alive.set(false)
                foregrounded = false
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
                Process.killProcess(Process.myPid())
            }, 1000)
            return response
        }
        override fun profile(recording: Boolean): Bundle =
            profileCapture.change(recording, Native.status().firstOrNull() ?: "stopped")
        override fun status(): Bundle {
            val s = Native.status()
            refreshNetworkSettings()
            val network = networkSession.snapshot()
            val proxy = Native.networkStatus()
            s.getOrNull(2)?.takeIf { it.isNotEmpty() }?.let { error = it }
            if (s.firstOrNull() == "failed" && started.isConsumed() && failureRecorded.compareAndSet(false, true)) {
                val retainedError = error.ifEmpty { "Guest worker failed; writable state and diagnostics retained. Start a fresh worker." }
                Thread({ persistStopError(retainedError); persistObservations("unexpected-engine-exit") }, "guest-failure-record").start()
            }
            return Bundle().apply {
                putAll(profileCapture.snapshot(s.getOrElse(0) { "stopped" }))
                putString("state", s.getOrElse(0) { "stopped" })
                putString("detail", s.getOrElse(1) { "" })
                putString("engineLog", s.getOrElse(3) { "" })
                putString("debugQmpPath", s.getOrElse(4) { "" })
                putString("graphicsProviderChoice", s.getOrElse(5) { "platform" })
                putBoolean("graphicsProviderFixed", s.getOrElse(6) { "0" } == "1")
                putBoolean("touchTraceConfigured", s.getOrElse(7) { "0" } == "1")
                putBoolean("vcpuAccountingConfigured", s.getOrElse(15) { "0" } == "1")
                // Request only. Actual hits/coverage are established by engine logs.
                putBoolean("experimentalTcgIrCacheRequested", s.getOrElse(16) { "0" } == "1")
                putLong("displayInputGeneration", s.getOrElse(13) { "0" }.toLongOrNull() ?: 0L)
                putLong("guestFrames", s.getOrElse(17) { "-1" }.toLongOrNull() ?: -1L)
                putIntArray("guestFrameBuckets", Native.guestFrameBuckets())
                putLong("displayInputOwner", s.getOrElse(14) { "0" }.toLongOrNull() ?: 0L)
                val shutdown = ShutdownNativeSnapshot.from(s)
                putBoolean("nativePollSnapshotValid", shutdown.valid)
                putBoolean("guestShutdownConfirmed", shutdown.confirmed)
                shutdown.dirty?.let { putBoolean("nativeStateDirty", it) }
                shutdown.storageFailed?.let { putBoolean("nativeStorageFailed", it) }
                shutdown.engineExit?.let { putInt("nativeEngineExit", it) }
                putString("error", error.ifEmpty { audioError })
                putInt("pid", Process.myPid())
                putInt("uid", Process.myUid())
                putString("usbMuxSocket", usb.muxSocket ?: "")
                putString("appServicesError", usbError)
                putString("networkStatus", if (!network.active.enabled) "Off" else "${network.active.summary} · $networkSetup" +
                    if (networkSetup == "Ready") " · ${proxy.firstOrNull().orEmpty()}" else "")
                putString("networkSetup", networkSetup)
                putBoolean("networkSetupError", networkSetupError)
                putString("networkActivity", proxy.firstOrNull().orEmpty())
                putLong("networkConnections", proxy.getOrNull(2)?.toLongOrNull() ?: 0)
                putLong("networkCompleted", proxy.getOrNull(4)?.toLongOrNull() ?: 0)
                putLong("networkFailed", proxy.getOrNull(5)?.toLongOrNull() ?: 0)
                putString("networkLastResult", proxy.getOrElse(6) { "" })
                putBoolean("networkLastResultError", proxy.getOrNull(7) == "1")
                putInt("networkMode", network.active.nativeMode)
                putString("networkArchiveDate", network.active.archiveDate)
                putBoolean("networkLocal", network.active.localNetwork)
                putString("networkChangeNotice", network.notice)
                putString("networkCertificateFingerprint", proxy.getOrElse(1) { "" })
                putBoolean("sessionStarted", started.isConsumed())
                putBundle("observations", observations())
            }
        }
        override fun claimDisplay(): Long = Native.claimDisplay()
        override fun displayInputGeneration(lease: Long): Long = Native.displayInputGeneration(lease)
        override fun attachDisplay(lease: Long, surface: Surface): String = try {
            Native.attachDisplay(lease, surface)
        } finally { surface.release() } // Includes rejected stale parcels.
        override fun releaseDisplay(lease: Long) { Native.releaseDisplay(lease) }
        override fun detachDisplay(lease: Long) { Native.detachDisplay(lease) }
        override fun cancelDisplayInput(lease: Long) { Native.cancelDisplayInput(lease, 0L) }
        override fun backgroundDisplay(lease: Long) { lifecycleAdmission.run {
            if (stopping.get()) return@run
            // Keep the sink monitor across the authoritative native owner check.
            // Stale owners change nothing; no write can cross accepted pause
            // admission before the epoch closes and the retained track flushes.
            val result = audioGuard.closeAfterAdmission({ Native.backgroundDisplay(lease) }) {
                userPaused = true
                audioObservations.pauses++
                flushAudioLocked()
            }
            if (result == "Display ownership retired") return@run
            if (result.isNotEmpty()) { error = result; return@run }
            stopAudioOutput()
            updateNotification()
        } }
        override fun pause(paused: Boolean) { lifecycleAdmission.run {
            // A background callback can race the stop button. Shutdown already
            // cancels input; an inactive pause must not replace its diagnostic.
            if (stopping.get()) return@run
            userPaused = paused
            if (!paused) focusPaused = false
            if (paused) pauseAudio() // Publish/flush the sink fence before native admission.
            Native.cancelInput()
            val result = Native.pause(paused)
            if (result.isNotEmpty()) error = result
            synchronized(audioGuard) { if (paused) audioObservations.pauses++ else audioObservations.resumes++ }
            if (paused) stopAudioOutput()
            else startAudioOutput()
            updateNotification()
        } }
        override fun stop() {
            if (!stopping.compareAndSet(false, true)) return
            updateNotification()
            profileCapture.finish("guest-stop")
            shutdownWatchdog.request()
        }
        override fun button(button: Int, down: Boolean) {
            val result = Native.button(button, down)
            if (result.isNotEmpty()) { error = result; buttonRejections.incrementAndGet() }
            else if (button in 0..1) (if (down) buttonDownAdmissions else buttonUpAdmissions).incrementAndGet(button)
        }
        override fun touch(timestampNanos: Long, ids: IntArray, phases: IntArray, coordinates: FloatArray, width: Int, height: Int, rotation: Int): String =
            admitTouch(null, 0L, timestampNanos, ids, phases, coordinates, width, height, rotation).also { displayCancellations.record(it) }
        override fun touchDisplay(lease: Long, inputGeneration: Long, timestampNanos: Long, ids: IntArray, phases: IntArray, coordinates: FloatArray, width: Int, height: Int, rotation: Int): String =
            lifecycleAdmission.run {
                // Focus loss and gain may finish between UI status polls. Do not
                // admit a new gesture while host pause/focus cleanup is active.
                if (userPaused || focusPaused || stopping.get()) DisplayInputAdmission.PAUSED
                else admitTouch(lease, inputGeneration, timestampNanos, ids, phases, coordinates, width, height, rotation)
            }.also { displayCancellations.record(it) }
        private fun admitTouch(lease: Long?, inputGeneration: Long, timestampNanos: Long, ids: IntArray, phases: IntArray, coordinates: FloatArray, width: Int, height: Int, rotation: Int): String = synchronized(touchAdmissionGuard) {
            // Native admission rechecks the owner under the control guard. This
            // early check only avoids advancing the clock for already stale Views.
            if (lease != null && !Native.ownsDisplay(lease)) return@synchronized "Display ownership retired"
            if (lease != null && (inputGeneration <= 0L || Native.displayInputGeneration(lease) != inputGeneration))
                return@synchronized DisplayInputAdmission.GENERATION_RETIRED
            val ordered = try { touchTimestampOrder.select(timestampNanos) }
            catch (failure: IllegalArgumentException) {
                val rejected = "${failure.message}; gesture cancelled. Start a new gesture."
                if (lease == null) { error = rejected; Native.cancelInput() }
                else {
                    // Cancellation may advance between the early generation
                    // read and timestamp validation. Never cancel newer input.
                    val release = Native.cancelDisplayInput(lease, inputGeneration)
                    if (release == "Display ownership retired" || DisplayInputAdmission.isCancellation(release))
                        return@synchronized DisplayInputAdmission.GENERATION_RETIRED
                    val failureMessage = if (release.isEmpty()) rejected else "$rejected; input release failed: $release"
                    error = failureMessage
                    return@synchronized failureMessage
                }
                return@synchronized rejected
            }
            val result = if (lease == null) Native.touch(ordered, ids, phases, coordinates, width, height, rotation)
                else Native.touchDisplay(lease, inputGeneration, ordered, ids, phases, coordinates, width, height, rotation)
            if (result.isNotEmpty() && result != "Display ownership retired" && !DisplayInputAdmission.isCancellation(result)) error = result
            result
        }
        override fun cancelInput() { Native.cancelInput() }
        override fun motion(kind: Int, timestampNanos: Long, xyz: FloatArray, rotation: Int) {
            val result = Native.motion(kind, timestampNanos, xyz, rotation)
            if (result.isNotEmpty()) error = result
        }
        override fun guestRequest(operation: Int, argument: String, timeoutMs: Int): Bundle {
            val result = Native.guestRequest(operation, argument, timeoutMs)
            return Bundle().apply { putLong("requestId", result[0].toLong()); putString("error", result[1]) }
        }
        override fun guestPoll(requestId: Long): Bundle {
            val result = Native.guestPoll(requestId)
            return Bundle().apply { putInt("state", result[0].toInt()); putInt("guestStatus", result[1].toInt()); putString("output", result[2]); putString("error", result[3]) }
        }
        override fun guestCancel(requestId: Long) { Native.guestCancel(requestId) }
        override fun screenshot(path: String): String = try {
            val target = File(path).canonicalFile
            require(target.path.startsWith(File(filesDir, "screenshots").canonicalPath + "/"))
            target.parentFile?.mkdirs()
            val bitmap = Native.capture() ?: error("No guest frame available")
            target.outputStream().use { bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it) }
            bitmap.recycle()
            ""
        } catch (e: Exception) { e.message ?: "Screenshot failed" }
    }
    private fun refreshNetworkSettings() {
        if (stopping.get()) return
        networkSession.refresh { config ->
            val result = Native.networkUpdate(config.nativeMode, config.archiveDate, config.localNetwork)
            (result.getOrNull(0)?.toIntOrNull() ?: 2) to result.getOrElse(1) { "Network worker unavailable" }
        }
        // Turned on live for the first time this session: set the guest up as a networked start does.
        if (networkSession.snapshot().active.enabled) startGuestNetwork()
    }

    private fun startGuestNetwork() {
        if (!guestNetworkStarted.compareAndSet(false, true)) return
        networkSetup = "Waiting for guest tools…"
        networkThread.execute { configureGuestNetwork() }
    }

    /** Fixed typed operations only; no shell or arbitrary guest commands. A failed
     * mutation is reported once and is never silently replayed after a timeout. */
    private fun configureGuestNetwork() {
        networkSetupError = false
        try {
            val operations = GuestNetworkOperation(Native::guestRequest, Native::guestPoll, Native::guestCancel,
                { alive.get() && !stopping.get() }, SystemClock::elapsedRealtime, Thread::sleep)
            fun operation(kind: Int, argument: String = "") = operations.execute(kind, argument)
            operations.execute(0, admissionWaitMs = 120_000)
            // Use the app's matching signed helpers, including on older installed
            // packages that have an agent but omit ittrust/itproxy. Chunk uploads
            // are atomic in the guest and never replayed after a lost completion.
            networkSetup = "Preparing guest network tools…"
            for ((name, kind) in listOf("ittrust" to 9, "itproxy" to 10)) {
                assets.open("guest-tools/$name").use { stream ->
                    val binary = stream.readBytes()
                    check(binary.size in 1..1_048_576) { "Bundled network tool is invalid: $name" }
                    var offset = 0
                    while (offset < binary.size) {
                        val end = minOf(offset + 2048, binary.size)
                        val chunk = android.util.Base64.encodeToString(binary.copyOfRange(offset, end), android.util.Base64.NO_WRAP)
                        operation(kind, "$offset ${if (end == binary.size) 1 else 0} $chunk")
                        offset = end
                    }
                }
            }
            networkSetup = "Installing guest certificate…"
            operation(6, android.util.Base64.encodeToString(Native.networkCertificate(), android.util.Base64.NO_WRAP))
            val trusted = operation(7)
            check(trusted.contains("Guest trust add: 0")) { "The guest did not accept its HTTPS certificate. Update guest tools and restart." }
            networkSetup = "Configuring the guest connection…"
            check(operation(8).contains("Proxy enabled")) { "The guest did not accept the proxy. Update guest tools and restart." }
            networkSetup = "Ready"
        } catch (error: Exception) {
            networkSetup = error.message ?: "Network setup failed. Update guest tools and restart."
            networkSetupError = true
        }
    }

    @Suppress("DEPRECATION")
    private fun persistObservations(reason: String) {
        fun encode(bundle: Bundle): JSONObject = JSONObject().apply {
            for (key in bundle.keySet()) when (val value = bundle.get(key)) { is Bundle -> put(key, encode(value)); else -> put(key, value) }
        }
        try {
            val receipt = encode(binder.status()).put("reason", reason).put("hostElapsedMs", SystemClock.elapsedRealtime())
                .put("audiblePlaybackVerified", false).put("visibleGuestVerified", false)
            File(filesDir, "worker-observations-${Process.myPid()}.json").writeText(receipt.toString(2))
        } catch (_: Exception) { /* State markers and per-worker native logs remain authoritative. */ }
    }
    /** Cached Java facts only, callable while the stop admission thread is blocked. */
    private fun shutdownProgressBundle(snapshot: ShutdownDebugProgress): Bundle {
        val observedAt = SystemClock.elapsedRealtime()
        fun call(value: ShutdownCallProgress) = Bundle().apply {
            putLong("entered", value.entered); putLong("returned", value.returned); putLong("threw", value.threw)
            value.lastEnteredAtMs?.let { putLong("lastEnteredAtHostMs", it) }
            value.lastReturnedAtMs?.let { putLong("lastReturnedAtHostMs", it) }
            value.lastException?.let { putString("lastException", it) }
        }
        return Bundle().apply {
            putInt("workerPid", Process.myPid()); putInt("workerUid", Process.myUid())
            putString("clock", "android.SystemClock.elapsedRealtime/BOOTTIME")
            putLong("observedAtHostMs", observedAt); putLong("revision", snapshot.revision)
            putBoolean("cachedJavaProgressOnly", true); putBoolean("shutdownConfirmationInferred", false)
            putBoolean("diagnosticPublicationBestEffort", true)
            putBoolean("terminalReceiptAuthority", false); putBoolean("requested", snapshot.requested)
            snapshot.updatedAtMs?.let { putLong("snapshotUpdatedAtHostMs", it); putLong("snapshotAgeMs", (observedAt - it).coerceAtLeast(0)) }
            snapshot.acceptedAtMs?.let { putLong("acceptedAtHostMs", it) }
            snapshot.deadlineMs?.let { putLong("deadlineHostMs", it) }
            putString("phase", snapshot.phase); putString("progressError", snapshot.error)
            snapshot.phaseEnteredAtMs?.let { putLong("phaseEnteredAtHostMs", it) }
            snapshot.phaseLastReturnedAtMs?.let { putLong("phaseLastReturnedAtHostMs", it) }
            putBundle("stopCall", call(snapshot.stop)); putBundle("cancelCall", call(snapshot.cancel))
            putBundle("statusCall", call(snapshot.status)); putBundle("receiptCall", call(snapshot.receipt))
            putBoolean("nativeStatusEverReturned", snapshot.lastReturnedStatus != null)
            snapshot.lastReturnedStatus?.let { returned -> putBundle("lastReturnedNativeStatus", Bundle().apply {
                putLong("returnedAtHostMs", returned.returnedAtMs); putLong("ageMs", (observedAt - returned.returnedAtMs).coerceAtLeast(0))
                putLong("pollNumber", returned.pollNumber); putString("state", returned.state); putString("error", returned.error)
                putBoolean("pollSnapshotValid", returned.native.valid); putBoolean("guestShutdownConfirmed", returned.native.confirmed)
                returned.native.dirty?.let { putBoolean("dirty", it) }
                returned.native.storageFailed?.let { putBoolean("storageFailed", it) }
                returned.native.engineExit?.let { putInt("engineExit", it) }
                putBoolean("cachedOnlyNotCurrentPoll", true); putBoolean("flagsRequireValidSnapshot", true)
            }) }
            snapshot.disposalReason?.let { putString("disposalReason", it) }
            snapshot.disposalRequestedAtMs?.let { putLong("disposalRequestedAtHostMs", it) }
        }
    }
    private fun persistStopError(message: String) {
        try { File(filesDir, "worker-stop-error-${Process.myPid()}.txt").writeText(message) } catch (_: java.io.IOException) { /* State markers remain authoritative. */ }
        try { File(filesDir, "worker-stop-error.txt").writeText(message) } catch (_: java.io.IOException) { /* Legacy private diagnostic only, not another worker's live status. */ }
    }
    private fun shutdownMessage(receipt: ShutdownReceipt): String = when {
            receipt.reason == "deadline-unconfirmed" -> "Guest shutdown was not confirmed within ${BuildConfig.SHUTDOWN_GRACE_MS / 1000} seconds; worker disposed and writable state retained. ${receipt.progress.error}"
            receipt.terminalState == "failed" -> "Guest worker reported failure; writable state retained: ${receipt.progress.error}"
            !receipt.cleanConfirmed -> "Native terminal shutdown was not explicitly confirmed clean; writable state and diagnostics retained. ${receipt.progress.error}"
            else -> receipt.progress.error
        }.take(4096)
    private fun shutdownRecord(receipt: ShutdownReceipt): JSONObject {
        val native = receipt.nativeSnapshot
        return JSONObject().put("reason", receipt.reason).put("admissionStage", receipt.progress.stage)
            .put("error", shutdownMessage(receipt)).put("nativeTerminalState", receipt.terminalState ?: JSONObject.NULL)
            .put("acceptedAtHostMs", receipt.acceptedAtMs).put("deadlineHostMs", receipt.deadlineMs).put("observedAtHostMs", receipt.observedAtMs)
            .put("workerPid", Process.myPid()).put("workerUid", Process.myUid())
            .put("nativePollSnapshotValid", native?.valid == true).put("guestShutdownConfirmed", native?.confirmed == true)
            .put("nativeStateDirty", native?.dirty ?: JSONObject.NULL).put("nativeStorageFailed", native?.storageFailed ?: JSONObject.NULL)
            .put("nativeEngineExit", native?.engineExit ?: JSONObject.NULL).put("cleanShutdownConfirmed", receipt.cleanConfirmed)
            .put("shutdownConfirmationInferred", false).put("nativeStateMarkersModified", false)
    }
    /** Admission-thread IO only; the native-free deadline stays armed throughout. */
    private fun persistTerminalReceipt(receipt: ShutdownReceipt) {
        val target = File(filesDir, "worker-shutdown-${Process.myPid()}.json")
        val temporary = File(filesDir, ".worker-shutdown-${Process.myPid()}.pending")
        java.io.FileOutputStream(temporary).use { output ->
            android.system.Os.chmod(temporary.absolutePath, 0x180) // 0600, own app only.
            output.write(shutdownRecord(receipt).put("receiptPersistenceBestEffort", false).toString(2).toByteArray(Charsets.UTF_8))
            output.fd.sync()
        }
        android.system.Os.rename(temporary.absolutePath, target.absolutePath)
        val directory = android.system.Os.open(filesDir.absolutePath,
            android.system.OsConstants.O_RDONLY or android.system.OsConstants.O_CLOEXEC, 0)
        try { android.system.Os.fsync(directory) } finally { android.system.Os.close(directory) }
    }
    private fun disposeAfterShutdown(receipt: ShutdownReceipt) {
        alive.set(false)
        val message = shutdownMessage(receipt)
        error = message
        if (receipt.reason == "native-terminal") {
            // Its immutable receipt was persisted before the coordinator's CAS.
            Process.killProcess(Process.myPid())
            return
        }
        disposeWithoutWaitingForReceipt(writeReceipt = {
            if (message.isNotEmpty()) persistStopError(message)
            // Deadline evidence is separate: it cannot overwrite terminal facts.
            // No Native, audio/framework cleanup or synchronous IO precedes kill.
            val record = shutdownRecord(receipt).put("receiptPersistenceBestEffort", true)
            try { File(filesDir, "worker-shutdown-deadline-${Process.myPid()}.json").writeText(record.toString(2)) }
            catch (_: java.io.IOException) { /* Retained state markers stay authoritative. */ }
        }, terminate = { Process.killProcess(Process.myPid()) })
    }
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int { foreground(); return START_NOT_STICKY }
    override fun onBind(intent: Intent): IBinder = binder
    override fun onCreate() {
        super.onCreate()
        // A worker that ended its device is replaced by a fresh one while LightTouch stays bound, and Android carries
        // the old one's foreground state over: its "running" notification stayed up with nothing running.
        stopForeground(STOP_FOREGROUND_REMOVE)
        detailedTimingThread.scheduleWithFixedDelay({
            try {
                if (detailedTiming && detailedTimingError.isEmpty() && !stopping.get())
                    detailedTimingError = profileCapture.startAutomatic(Native.status().firstOrNull() ?: "stopped")
            }
            catch (_: Exception) { /* Retried on the next tick; the overlay shows timing as unavailable. */ }
        }, 3, 3, java.util.concurrent.TimeUnit.SECONDS)
        // A fresh worker must not inherit a prior PID's error as current status.
        // The UI reads the exited PID's receipt/error; state markers and legacy
        // diagnostics remain available for cold recovery review.
        Thread({
            val samples = ShortArray(8192)
            var format = ""
            while (alive.get()) {
                // This ticket predates both the native PCM read and format read.
                // Pause/resume can run while either JNI call is outside the sink lock.
                val packetEpoch = audioGuard.capture()
                val count = Native.audio(samples)
                if (count < 0) {
                    synchronized(audioGuard) { audioObservations.nativePullFailures++ }
                    audioError = "Guest audio could not be read (status ${-count}). Stop and restart the guest."
                    Thread.sleep(100)
                    continue
                }
                if (count > 0) synchronized(audioGuard) { audioObservations.samples(samples, 0, count, false) }
                if (count > 0 && audioOn && !focusPaused && !userPaused && !stopping.get()) {
                    try {
                        val spec = Native.audioInfo()
                        require(spec[0] > 0 && spec[1] in 1..2) { "Invalid guest audio format" }
                        val next = spec.joinToString()
                        synchronized(audioGuard) {
                            if (!audioGuard.isCurrent(packetEpoch) || !audioOn || focusPaused || userPaused || stopping.get()) {
                                audioObservations.discardedReadPackets++; audioObservations.discardedReadSamples += count
                                return@synchronized
                            }
                            if (next != format) {
                                audio?.let { audioObservations.observeTrack(it.playbackHeadPosition, it.underrunCount); it.release() }
                                val mask = if (spec[1] == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO
                                val minimum = AudioTrack.getMinBufferSize(spec[0], mask, AudioFormat.ENCODING_PCM_16BIT)
                                require(minimum > 0) { "Guest audio format unavailable" }
                                audio = AudioTrack.Builder().setAudioAttributes(audioAttributes()).setAudioFormat(
                                    AudioFormat.Builder().setSampleRate(spec[0]).setChannelMask(mask).setEncoding(AudioFormat.ENCODING_PCM_16BIT).build()
                                ).setBufferSizeInBytes(maxOf(minimum, count * 4)).build()
                                check(audio!!.state == AudioTrack.STATE_INITIALIZED) { "Audio output did not initialize" }
                                audioObservations.trackCreated(spec[0], spec[1]); audio!!.play(); format = next
                            }
                            var offset = 0
                            // A positive short write retains the unwritten tail. Stop,
                            // pause and focus loss are checked before each retry.
                            while (offset < count && audioGuard.isCurrent(packetEpoch) && alive.get() && audioOn && !focusPaused && !userPaused && !stopping.get()) {
                                val requested = count - offset
                                val written = audio!!.write(samples, offset, requested, AudioTrack.WRITE_BLOCKING)
                                audioObservations.writeCalls++; audioObservations.lastWriteCode = written
                                if (written <= 0) {
                                    audioObservations.writeFailures++
                                    audioError = "Guest audio output stopped (code $written). Stop and restart if it does not recover."
                                    audio?.release(); audio = null; format = ""
                                    break
                                }
                                if (written < requested) audioObservations.shortWrites++
                                audioObservations.samples(samples, offset, written, true); offset += written; audioError = ""
                            }
                            audio?.let { audioObservations.observeTrack(it.playbackHeadPosition, it.underrunCount) }
                        }
                    } catch (failure: Exception) {
                        synchronized(audioGuard) { audioObservations.writeFailures++; audio?.release(); audio = null; format = "" }
                        audioError = "Guest audio output failed: ${failure.message ?: "unknown error"}. Stop and restart the guest."
                        Thread.sleep(100)
                    }
                } else {
                    if (count > 0) synchronized(audioGuard) {
                        audioObservations.discardedReadPackets++; audioObservations.discardedReadSamples += count
                    }
                    Thread.sleep(10)
                }
            }
            synchronized(audioGuard) { audio?.release(); audio = null }
        }, "guest-audio").start()
    }
    private fun pauseAudio() = synchronized(audioGuard) {
        audioGuard.pause()
        flushAudioLocked()
    }
    /** Caller holds audioGuard; no JNI call or display ownership change here. */
    private fun flushAudioLocked() {
        audio?.let { audioObservations.observeTrack(it.playbackHeadPosition, it.underrunCount); it.pause(); it.flush(); audioObservations.flushed() }
    }
    private fun playAudio() = synchronized(audioGuard) { audio?.play(); audioGuard.resume() }
    private fun observations() = Bundle().apply {
        val values = Native.observations()
        val keys = listOf("hostMonotonicNs", "presentedFrames", "firstPresentHostNs", "lastPresentHostNs", "maxPresentGapNs", "maxPresentWorkNs",
            "windowFailures", "frameCopyFailures", "surfaceGeneration", "guestFrameSequence", "guestFrameTimestampNs", "nativePcmFailures", "pcmDroppedFrames",
            "presentIntervalsTo16ms", "presentIntervalsTo33ms", "presentIntervalsTo50ms", "presentIntervalsTo100ms", "presentIntervalsTo250ms", "presentIntervalsOver250ms", "producedFrames", "producerFrameSequence", "producerFrameTimestampNs",
            "framePublishedHostNs", "pcmCapturedFrames", "pcmPulledFrames", "sharedPcmDroppedFrames", "inputRequests", "pcmQueuedPackets", "firstWindowLockError", "lastWindowLockError", "firstWindowPostError", "lastWindowPostError", "lastWindowGeometryError",
            "windowGeometryFailures", "windowBufferFormat", "windowBufferWidth", "windowBufferHeight", "windowBufferStride", "invalidWindowBuffers", "firstWindowErrorHostNs", "runtimeDiagnosticsStatus", "engineReady", "displaySleeping", "backlightLevel",
            "guestAgentSampleStatus", "guestAgentState", "shutdownPhase", "shutdownRoute", "shutdownReplyStatus", "shutdownReplyReceived", "guestFrameWidth", "guestFrameHeight", "guestFrameSamplePixels", "guestFrameNonzeroRgbSamples", "guestFrameRgbSampleSum",
            "windowAttached", "presentationEpoch", "presenterPhase", "presenterPhaseEntryHostNs", "presenterPhaseGeneration", "presenterPhaseEpoch", "presenterPhaseRevision", "presenterPhaseSampleStable",
            "ignoredInputRejections", "lastIgnoredInputRequest")
        for ((index, key) in keys.withIndex()) putLong(key, values[index])
        putBoolean("presenterTelemetryGuestEvidence", false)
        val diagnosticAvailable = getLong("runtimeDiagnosticsStatus") == 0L
        val agentAvailable = getLong("guestAgentSampleStatus") == 0L
        putBoolean("runtimeDiagnosticsAvailable", diagnosticAvailable)
        putBoolean("guestAgentStateAvailable", agentAvailable)
        putBoolean("guestAgentEndpointAvailable", false)
        putBoolean("guestAgentFreshnessAgeAvailable", false)
        putString("guestAgentStateLabel", when {
            !agentAvailable -> "unavailable"
            !diagnosticAvailable || getLong("engineReady") == 0L -> "not ready"
            getLong("guestAgentState") == 1L -> "alive"
            getLong("guestAgentState") == 2L -> "stale"
            else -> "absent"
        })
        putBundle("audio", synchronized(audioGuard) { audioObservations.snapshot().apply {
            putLong("packetGeneration", audioGuard.currentGeneration()); putBoolean("packetAdmissionOpen", audioGuard.admissionOpen())
        } })
        putBoolean("userPaused", userPaused); putBoolean("focusPaused", focusPaused); putBoolean("audioOutputOn", audioOn)
        putLongArray("buttonDownAdmissions", LongArray(2) { buttonDownAdmissions.get(it) })
        putLongArray("buttonUpAdmissions", LongArray(2) { buttonUpAdmissions.get(it) })
        putLong("buttonRejections", buttonRejections.get())
        // This worker-owned history survives View replacement. It reports
        // rejected/cancelled host batches, never guest application or a pass.
        val cancellations = displayCancellations.snapshot
        putBundle("displayInputCancellations", Bundle().apply {
            putLong("count", cancellations.count)
            putString("lastReason", cancellations.reasons.lastOrNull() ?: "")
            // Nested Bundle encoding is shared by status receipts and durable
            // observations. Indexed strings avoid an opaque raw-array value.
            putBundle("reasons", Bundle().apply {
                cancellations.reasons.forEachIndexed { index, reason -> putString(index.toString(), reason) }
            })
            putInt("historyLimit", DisplayCancellationHistory.LIMIT)
            putBoolean("guestConsumptionInferred", false)
        })
        // Volatile immutable snapshot: observers never wait on touch/native admission.
        val touchClock = touchTimestampOrder.snapshot
        putBundle("touchTimestampSelection", Bundle().apply {
            putLong("rawUptimeNs", touchClock.rawUptimeNs); putLong("orderedUptimeNs", touchClock.orderedUptimeNs)
            putLong("selections", touchClock.selections); putLong("adjustedSelections", touchClock.adjustedSelections)
            putLong("rejectedSelections", touchClock.rejectedSelections); putBoolean("guestConsumptionInferred", false)
        })
    }
    private fun audioAttributes() = AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_GAME).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build()
    /**
     * Guest audio starts with the guest and mixes with whatever else plays: LightTouch requests no audio focus, so
     * music or a video playing in the background carries on, and another app's audio never pauses the guest.
     */
    private fun startAudioOutput() { lifecycleAdmission.run {
        if (userPaused || stopping.get()) return@run
        if (audioOn) { playAudio(); return@run }
        synchronized(audioGuard) {
            // Resume the retained track before the audio thread sees it on.
            // Backgrounding pauses it without changing the PCM format.
            audio?.play()
            audioGuard.resume()
            audioOn = true
        }
    } }
    private fun stopAudioOutput() { lifecycleAdmission.run { audioOn = false } }
    /** The ongoing notification while a device is on in this worker (Android requires one to keep it running). */
    private fun foreground() {
        startForeground(1, notification())
        foregrounded = true
    }
    /** The notification follows the device: running, paused (LightTouch left), or shutting down. */
    private fun updateNotification() {
        if (foregrounded) getSystemService(NotificationManager::class.java).notify(1, notification())
    }
    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel("guest", "Running device", NotificationManager.IMPORTANCE_LOW))
        val launch = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val text = when {
            stopping.get() -> "Shutting down"
            userPaused -> "Paused while you're away. Tap to go back to it."
            !started.isConsumed() -> "Starting"
            else -> "Running. Tap to go back to it."
        }
        return Notification.Builder(this, "guest").setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle(deviceName.ifEmpty { "LightTouch" }).setContentText(text).setContentIntent(launch).setOngoing(true).build()
    }
    override fun onDestroy() { networkThread.shutdownNow(); detailedTimingThread.shutdownNow(); usb.stop(); performanceSampler.close(); alive.set(false); Native.cancelInput(); stopAudioOutput(); super.onDestroy() }
}
