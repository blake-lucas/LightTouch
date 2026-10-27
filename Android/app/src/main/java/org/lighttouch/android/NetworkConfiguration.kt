package org.lighttouch.android

import android.content.Context
import android.util.AtomicFile
import org.json.JSONObject
import java.io.File
import java.time.LocalDate
import java.time.format.DateTimeFormatter

enum class NetworkRoute(val label: String) { DIRECT("Direct internet"), ARCHIVE("Wayback Machine") }

/** A calendar day, independent of the host or guest's timezone and clock. */
data class NetworkConfiguration(
    val enabled: Boolean = false,
    val route: NetworkRoute = NetworkRoute.DIRECT,
    val archiveDate: String = "20090909",
    val localNetwork: Boolean = false,
) {
    val nativeMode: Int get() = if (!enabled) 0 else if (route == NetworkRoute.DIRECT) 1 else 2
    val date: LocalDate get() = LocalDate.parse(archiveDate, DateTimeFormatter.BASIC_ISO_DATE)
    val summary: String get() = if (!enabled) "Off" else if (route == NetworkRoute.DIRECT) "Direct internet" else "Wayback · ${date.format(DateTimeFormatter.ofPattern("d MMM yyyy"))}"
    fun validate() {
        require(archiveDate.length == 8 && archiveDate.all { it in '0'..'9' } && date >= LocalDate.of(1996, 1, 1) && date <= LocalDate.now()) { "Choose an archive date between 1996 and today." }
    }
    companion object {
        // SharedPreferences caches are process-local. The UI writes one atomic document;
        // a newly started engine reads the complete configuration even if it bound earlier.
        private fun file(context: Context) = AtomicFile(File(context.filesDir, "network.json"))
        fun load(context: Context): NetworkConfiguration = runCatching {
            val bytes = file(context).readFully()
            require(bytes.size <= 4096)
            val json = JSONObject(bytes.toString(Charsets.UTF_8))
            NetworkConfiguration(json.optBoolean("enabled", false),
                NetworkRoute.valueOf(json.optString("route", "DIRECT")),
                json.optString("archiveDate", "20090909"), json.optBoolean("localNetwork", false)).also { it.validate() }
        }.getOrDefault(NetworkConfiguration()) // missing/corrupt preferences always fail closed
        fun save(context: Context, config: NetworkConfiguration) {
            config.validate()
            val json = JSONObject().put("enabled", config.enabled).put("route", config.route.name)
                .put("archiveDate", config.archiveDate).put("localNetwork", config.localNetwork)
            val target = file(context)
            val stream = target.startWrite()
            try { stream.write(json.toString().toByteArray()); target.finishWrite(stream) }
            catch (error: Exception) { target.failWrite(stream); throw error }
        }
    }
}
