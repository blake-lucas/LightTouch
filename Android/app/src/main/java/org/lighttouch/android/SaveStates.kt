package org.lighttouch.android

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Bundle
import android.os.Handler
import android.os.RemoteException
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** A save state of a device (Portable/src/States.cpp): its name, when it was saved, its size and its screen then. */
data class SaveStateSummary(val id: String, val name: String, val app: String, val createdMs: Long, val bytes: Long,
                            val thumbnail: String, val width: Int, val height: Int) {
    companion object {
        fun of(b: Bundle) = SaveStateSummary(b.getString("id") ?: "", b.getString("name") ?: "", b.getString("app") ?: "",
            b.getLong("createdMs"), b.getLong("bytes"), b.getString("thumbnail") ?: "", b.getInt("width"), b.getInt("height"))
    }
}

/** A save or load under way, with its progress. */
data class StateJob(val loading: Boolean, val done: Long, val total: Long) {
    val fraction: Float? get() = if (total > 0) (done.toDouble() / total).coerceIn(0.0, 1.0).toFloat() else null
    val label: String get() = if (loading) "Loading state" else "Saving state"
}

/**
 * The States menu's model, as the desktop's (lighttouch-model states.rs): the current device's states, saving the
 * running device, and loading a state, which ends a running worker without an iOS shutdown and starts a fresh one
 * from the state (the engine starts once per worker). The worker does the work; this keeps what the menu shows.
 */
class SaveStatesController(private val engine: () -> IEngine?, private val handler: Handler,
                           private val notify: (String) -> Unit, private val fail: (String) -> Unit) {
    var states by mutableStateOf<List<SaveStateSummary>>(emptyList())
        private set
    var job by mutableStateOf<StateJob?>(null)
        private set
    /** The record the list is of. */
    private var listed: String? = null
    private val thumbnails = HashMap<String, Bitmap?>()

    /** Read the current device's states (again, when [force]). */
    fun refresh(recordId: String?, force: Boolean = false) {
        if (recordId == null) { states = emptyList(); listed = null; return }
        if (!force && recordId == listed) return
        listed = recordId
        val worker = engine() ?: return
        Thread({
            val list = try { worker.states(recordId).map { SaveStateSummary.of(it) } } catch (_: RemoteException) { null }
            handler.post { if (list != null && listed == recordId) states = list }
        }, "save-states-list").start()
    }

    fun thumbnail(state: SaveStateSummary): Bitmap? =
        if (state.thumbnail.isEmpty()) null
        else thumbnails.getOrPut(state.thumbnail) { runCatching { BitmapFactory.decodeFile(state.thumbnail) }.getOrNull() }

    /** Save the running device: the worker saves in the background, and [poll] follows it. */
    fun save(app: String) {
        if (job != null) return
        val worker = engine() ?: return
        job = StateJob(loading = false, done = 0, total = 0)
        Thread({
            val result = try { worker.saveState("", app) } catch (e: RemoteException) { e.message ?: "The device isn't running" }
            if (result.isNotEmpty()) handler.post { job = null; fail(result) }
        }, "save-state-request").start()
    }

    /** A load starting: its progress shows until the device runs. */
    fun loading() { job = StateJob(loading = true, done = 0, total = 0) }

    /** Called from the status poll (on its thread): the progress of a save or load, and a save's outcome. */
    fun poll(worker: IEngine?, status: String) {
        val current = job ?: return
        // Not "stopped": a load reads that a moment between the old worker and the new one's session.
        if (current.loading && status in listOf("running", "paused", "failed")) { handler.post { job = null }; return }
        val progress = try { worker?.stateProgress() } catch (_: RemoteException) { null } ?: return
        val saved = progress.getString("saved"); val failed = progress.getString("failed")
        val phase = progress.getString("phase") ?: ""
        handler.post {
            when {
                saved != null -> { job = null; notify("Saved “$saved”"); listed?.let { refresh(it, force = true) } }
                failed != null -> { job = null; fail(failed) }
                phase.isNotEmpty() -> job = StateJob(loading = phase != "saving", done = progress.getLong("done"), total = progress.getLong("total"))
            }
        }
    }

    fun delete(recordId: String, state: SaveStateSummary) {
        val worker = engine() ?: return
        Thread({
            val result = try { worker.deleteState(recordId, state.id) } catch (e: RemoteException) { e.message ?: "Worker unavailable" }
            handler.post { if (result.isEmpty()) notify("Save state deleted") else fail("Couldn't delete the save state: $result"); refresh(recordId, force = true) }
        }, "save-state-delete").start()
    }

    fun rename(recordId: String, state: SaveStateSummary, name: String) {
        val worker = engine() ?: return
        Thread({
            val result = try { worker.renameState(recordId, state.id, name.trim()) } catch (e: RemoteException) { e.message ?: "Worker unavailable" }
            handler.post { if (result.isNotEmpty()) fail("Couldn't rename the save state: $result"); refresh(recordId, force = true) }
        }, "save-state-rename").start()
    }
}
