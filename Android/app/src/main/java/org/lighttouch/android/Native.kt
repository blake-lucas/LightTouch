package org.lighttouch.android

import android.view.Surface

/** Loaded only in :engine. UI never loads QEMU or constructs boot arguments. */
internal object Native {
    init { System.loadLibrary("lighttouch_jni") }
    external fun importBase(source: String, libraryRoot: String, displayName: String, rom: String, guest: String, sha256: String = ""): String
    /** A firmwarekit output folder in this app's storage, moved in as a new record's base (lt_library_adopt_prepared). */
    external fun adoptPrepared(source: String, libraryRoot: String, displayName: String, rom: String): String
    /** The running import's phase, bytes done and bytes total; an empty phase when none runs. */
    external fun importProgress(): Array<String>
    external fun setGuestPackage(libraryRoot: String, id: String, path: String): String
    external fun records(libraryRoot: String): Array<Array<String>>
    external fun select(libraryRoot: String, id: String): String
    external fun rename(libraryRoot: String, id: String, name: String): String
    external fun remove(libraryRoot: String, id: String): String
    external fun frameTimeline(since: Long): LongArray
    external fun storageStats(): LongArray
    external fun setPresentScale(mode: Int)
    external fun setPresentVisible(visible: Boolean)
    external fun networkPrepare(root: String, record: String, mode: Int, date: String, local: Boolean, roots: String): String
    external fun networkUpdate(mode: Int, date: String, local: Boolean): Array<String>
    external fun networkCertificate(): ByteArray
    external fun networkStatus(): Array<String>
    external fun start(libraryRoot: String, id: String, headphones: Boolean): String
    external fun recoverStart(libraryRoot: String, id: String, headphones: Boolean): String
    external fun status(): Array<String>
    external fun guestFrameBuckets(): IntArray
    external fun claimDisplay(): Long
    external fun ownsDisplay(lease: Long): Boolean
    external fun displayInputGeneration(lease: Long): Long
    external fun attachDisplay(lease: Long, surface: Surface): String
    external fun detachDisplay(lease: Long)
    external fun releaseDisplay(lease: Long)
    external fun cancelDisplayInput(lease: Long, inputGeneration: Long): String
    external fun backgroundDisplay(lease: Long): String
    external fun touchDisplay(lease: Long, inputGeneration: Long, time: Long, ids: IntArray, phases: IntArray, xy: FloatArray, width: Int, height: Int, rotation: Int): String
    external fun pause(paused: Boolean): String
    external fun stop(): String
    external fun shutdownGraceMillis(): Int
    external fun debugArmQmp(): String
    external fun debugEnableTouchTrace(): String
    external fun debugArmVcpuAccounting(): String
    external fun debugSelectTcgIrCache(enabled: Boolean): String
    external fun debugSelectGraphicsProvider(choice: Int): String
    external fun debugQmpSnapshot(): Array<String>
    external fun button(button: Int, down: Boolean): String
    external fun touch(time: Long, ids: IntArray, phases: IntArray, xy: FloatArray, width: Int, height: Int, rotation: Int): String
    external fun cancelInput()
    external fun motion(kind: Int, time: Long, xyz: FloatArray, rotation: Int): String
    external fun capture(): android.graphics.Bitmap?
    external fun observations(): LongArray
    external fun audioInfo(): IntArray
    external fun profile(path: String): String
    external fun profileStatus(): Int
    /** Request id (zero if unaccepted), error text, and portable lt_status code. */
    external fun guestRequest(operation: Int, argument: String, timeoutMs: Int): Array<String>
    external fun guestPoll(requestId: Long): Array<String>
    external fun guestCancel(requestId: Long)
    external fun audio(buffer: ShortArray): Int
}
