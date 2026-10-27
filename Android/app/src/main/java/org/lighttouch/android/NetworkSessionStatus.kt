package org.lighttouch.android

/** Applied session facts, kept separate from the editable preferences. */
data class NetworkSessionStatus(
    val available: Boolean = false,
    val mode: Int = 0,
    val archiveDate: String = "",
    val setup: String = "",
    val setupError: Boolean = false,
    val activity: String = "",
    val connections: Long = 0,
    val completed: Long = 0,
    val failed: Long = 0,
    val lastResult: String = "",
    val lastResultError: Boolean = false,
)
