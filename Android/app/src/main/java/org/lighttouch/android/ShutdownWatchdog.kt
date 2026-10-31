package org.lighttouch.android

import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

internal data class ShutdownProgress(val stage: String, val error: String = "")
/** Callback returns include neither engine acknowledgement nor durable receipt proof. */
internal data class ShutdownCallProgress(val entered: Long = 0, val returned: Long = 0,
    val threw: Long = 0, val lastEnteredAtMs: Long? = null, val lastReturnedAtMs: Long? = null,
    val lastException: String? = null)
internal data class ShutdownReturnedStatus(val returnedAtMs: Long, val pollNumber: Long,
    val state: String?, val error: String?, val native: ShutdownNativeSnapshot)
/** One immutable cached snapshot; reading it never calls native code or takes an admission lock. */
internal data class ShutdownDebugProgress(val revision: Long = 0, val updatedAtMs: Long? = null,
    val requested: Boolean = false, val acceptedAtMs: Long? = null, val deadlineMs: Long? = null,
    val phase: String = "not-requested", val phaseEnteredAtMs: Long? = null,
    val phaseLastReturnedAtMs: Long? = null, val error: String = "",
    val stop: ShutdownCallProgress = ShutdownCallProgress(), val cancel: ShutdownCallProgress = ShutdownCallProgress(),
    val status: ShutdownCallProgress = ShutdownCallProgress(), val receipt: ShutdownCallProgress = ShutdownCallProgress(),
    val lastReturnedStatus: ShutdownReturnedStatus? = null,
    val disposalReason: String? = null, val disposalRequestedAtMs: Long? = null)
/** Only the appended fields from the same native poll may establish clean shutdown. */
internal data class ShutdownNativeSnapshot(val valid: Boolean, val confirmed: Boolean,
    val dirty: Boolean?, val storageFailed: Boolean?, val engineExit: Int?) {
    fun clean(state: String?) = valid && state == "stopped" && confirmed && dirty == false && storageFailed == false && engineExit == 0
    companion object {
        fun from(status: Array<String>): ShutdownNativeSnapshot {
            fun flag(index: Int): Boolean? = when (status.getOrNull(index)) { "0" -> false; "1" -> true; else -> null }
            val confirmed = flag(9); val dirty = flag(10); val storage = flag(11); val exit = status.getOrNull(12)?.toIntOrNull()
            return ShutdownNativeSnapshot(flag(8) == true && confirmed != null && dirty != null && storage != null && exit != null,
                confirmed == true, dirty, storage, exit)
        }
    }
}
internal data class ShutdownReceipt(val reason: String, val progress: ShutdownProgress, val terminalState: String?,
    val acceptedAtMs: Long, val deadlineMs: Long, val observedAtMs: Long,
    val nativeSnapshot: ShutdownNativeSnapshot? = null) {
    val cleanConfirmed: Boolean get() = nativeSnapshot?.clean(terminalState) == true
}

/** Receipt persistence is best effort; filesystem waits cannot postpone disposal. */
internal fun disposeWithoutWaitingForReceipt(writeReceipt: () -> Unit, terminate: () -> Unit) {
    try {
        Thread({
            try { writeReceipt() } catch (_: Exception) { /* No confirmation is inferred from missing receipt bytes. */ }
        }, "guest-stop-receipt").apply { isDaemon = true }.start()
    } finally { terminate() }
}

/** The deadline never invokes native code and never waits for the admission thread. */
internal class ShutdownWatchdog(
    private val graceMs: Long,
    private val now: () -> Long,
    private val requestNativeStop: () -> String,
    private val cancelNativeContacts: () -> Unit,
    private val nativeStatus: () -> Array<String>,
    private val dispose: (ShutdownReceipt) -> Unit,
    private val persistTerminal: (ShutdownReceipt) -> Unit = {}
) {
    private val requested = AtomicBoolean(false)
    private val disposed = AtomicBoolean(false)
    private val progress = AtomicReference(ShutdownProgress("not-requested"))
    private val debugProgress = AtomicReference(ShutdownDebugProgress())
    private val timer = Executors.newSingleThreadScheduledExecutor { task -> Thread(task, "guest-stop-deadline").apply { isDaemon = true } }
    private var acceptedAt = 0L
    private var deadline = 0L

    fun cachedProgress(): ShutdownDebugProgress = debugProgress.get()

    private enum class Call(val phase: String) {
        STOP("shutdown-admission"), CANCEL("contact-cancellation"), STATUS("terminal-poll"), RECEIPT("terminal-receipt")
    }
    private inline fun publish(crossinline change: (ShutdownDebugProgress, Long) -> ShutdownDebugProgress) {
        // Observability must not prevent native admission or deadline disposal.
        try {
            val at = now()
            debugProgress.updateAndGet { old -> change(old, at).copy(revision = old.revision + 1, updatedAtMs = at) }
        } catch (_: Exception) { /* The prior immutable snapshot remains available, possibly stale. */ }
        catch (_: OutOfMemoryError) { /* No allocation-dependent diagnostic may hold up disposal. */ }
    }
    private fun setProgress(value: ShutdownProgress) {
        progress.set(value)
        publish { old, at -> old.copy(phase = value.stage, error = value.error,
            phaseEnteredAtMs = if (old.phase == value.stage) old.phaseEnteredAtMs else at,
            phaseLastReturnedAtMs = if (old.phase == value.stage) old.phaseLastReturnedAtMs else null) }
    }
    private fun ShutdownDebugProgress.call(kind: Call): ShutdownCallProgress = when (kind) {
        Call.STOP -> stop; Call.CANCEL -> cancel; Call.STATUS -> status; Call.RECEIPT -> receipt
    }
    private fun ShutdownDebugProgress.withCall(kind: Call, value: ShutdownCallProgress): ShutdownDebugProgress = when (kind) {
        Call.STOP -> copy(stop = value); Call.CANCEL -> copy(cancel = value)
        Call.STATUS -> copy(status = value); Call.RECEIPT -> copy(receipt = value)
    }
    private fun enter(kind: Call) = publish { old, at ->
        val call = old.call(kind)
        old.withCall(kind, call.copy(entered = call.entered + 1, lastEnteredAtMs = at))
    }
    private fun returned(kind: Call, failure: Exception? = null, state: Array<String>? = null) = publish { old, at ->
        val call = old.call(kind)
        val next = if (failure == null) call.copy(returned = call.returned + 1, lastReturnedAtMs = at, lastException = null)
            else call.copy(threw = call.threw + 1, lastReturnedAtMs = at, lastException = failure.toString().take(4096))
        old.withCall(kind, next).copy(
            phaseLastReturnedAtMs = if (old.phase == kind.phase) at else old.phaseLastReturnedAtMs,
            lastReturnedStatus = if (state == null) old.lastReturnedStatus else ShutdownReturnedStatus(
                at, next.returned, state.firstOrNull()?.take(64), state.getOrNull(2)?.take(4096), ShutdownNativeSnapshot.from(state)))
    }
    private fun <T> tracked(kind: Call, action: () -> T, returnedState: (T) -> Array<String>? = { null }): T {
        enter(kind)
        try {
            val value = action()
            returned(kind, state = returnedState(value))
            return value
        } catch (failure: Exception) { returned(kind, failure); throw failure }
    }

    fun request(): Boolean {
        if (!requested.compareAndSet(false, true)) return false
        require(graceMs > 0)
        acceptedAt = now(); deadline = acceptedAt + graceMs
        progress.set(ShutdownProgress("shutdown-admission"))
        publish { old, at -> old.copy(requested = true, acceptedAtMs = acceptedAt, deadlineMs = deadline,
            phase = "shutdown-admission", phaseEnteredAtMs = at) }
        // This must happen before the worker can initialize or call Native.
        timer.schedule({ finish("deadline-unconfirmed", null) }, graceMs, TimeUnit.MILLISECONDS)
        Thread({
            try {
                if (disposed.get()) return@Thread
                val admissionError = tracked(Call.STOP, requestNativeStop).take(4096)
                if (disposed.get()) return@Thread
                setProgress(ShutdownProgress("contact-cancellation", admissionError))
                tracked(Call.CANCEL, cancelNativeContacts)
                if (disposed.get()) return@Thread
                setProgress(ShutdownProgress("terminal-poll", admissionError))
                while (!disposed.get()) {
                    val state = tracked(Call.STATUS, nativeStatus) { it }
                    if (disposed.get()) return@Thread
                    if (now() >= deadline) { finish("deadline-unconfirmed", null); return@Thread }
                    if (state.firstOrNull() in listOf("stopped", "failed")) {
                        val reportedError = if (state.firstOrNull() == "failed") {
                            state.getOrNull(2)?.takeIf { it.isNotEmpty() }
                                ?: admissionError.takeIf { it.isNotEmpty() }
                                ?: state.getOrElse(1) { "Shutdown was not confirmed" }
                        } else admissionError
                        setProgress(ShutdownProgress("native-terminal", reportedError.take(4096)))
                        val terminal = ShutdownReceipt("native-terminal", progress.get(), state.firstOrNull(),
                            acceptedAt, deadline, now(), ShutdownNativeSnapshot.from(state))
                        // The timer remains armed and disposed remains false during IO.
                        // A blocked writer can never prevent deadline disposal.
                        setProgress(ShutdownProgress("terminal-receipt", reportedError.take(4096)))
                        tracked(Call.RECEIPT, { persistTerminal(terminal) })
                        if (now() >= deadline) { finish("deadline-unconfirmed", null); return@Thread }
                        finish(terminal); return@Thread
                    }
                    Thread.sleep(100)
                }
            } catch (failure: Exception) {
                // Admission/poll failure is not proof of guest termination.
                val stage = if (progress.get().stage == "terminal-receipt") "terminal-receipt-failed" else "native-call-failed"
                setProgress(ShutdownProgress(stage, failure.toString().take(4096)))
            }
        }, "guest-stop-admission").apply { isDaemon = true }.start()
        return true
    }

    /** The user's Force stop: dispose now, as the deadline would, whether or not a shutdown was requested. */
    fun force() {
        if (requested.compareAndSet(false, true)) { acceptedAt = now(); deadline = acceptedAt }
        finish("user-forced", null)
    }

    private fun finish(reason: String, state: String?) {
        finish(ShutdownReceipt(reason, progress.get(), state, acceptedAt, deadline, now()))
    }
    private fun finish(receipt: ShutdownReceipt) {
        if (!disposed.compareAndSet(false, true)) return
        publish { old, _ -> old.copy(disposalReason = receipt.reason, disposalRequestedAtMs = receipt.observedAtMs) }
        try { dispose(receipt) } finally { timer.shutdownNow() }
    }
}
