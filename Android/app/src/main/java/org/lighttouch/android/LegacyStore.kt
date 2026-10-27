package org.lighttouch.android

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.util.LruCache
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import java.security.MessageDigest
import javax.net.ssl.HttpsURLConnection

/** The guest a search or download is checked against: what it runs, not the Android host. */
data class StoreTarget(val productType: String, val ios: String, val arch: String, val family: String = "1") {
    companion object {
        private val armv6 = setOf("iPod1,1", "iPod2,1", "iPhone1,1", "iPhone1,2")
        fun of(productType: String, ios: String) =
            StoreTarget(productType, ios, if (productType in armv6) "armv6" else "armv7", if (productType.startsWith("iPad")) "2" else "1")
    }
}

/** A category (the API calls it a genre). */
data class StoreGenre(val id: String, val name: String, val count: Int)
data class StoreCollection(val slug: String, val title: String, val subtitle: String?, val count: Int)
/** App rows from a listing; [next] is the address of the following page, which only the paged listings have. */
data class StorePage(val rows: List<JSONObject>, val total: Int, val next: String?)
/** One chart position. [app] is null for an app the archive has not preserved. */
data class ChartRow(val position: Int, val name: String, val developer: String?, val app: JSONObject?)
/** One archived chart snapshot: [date] is the snapshot nearest the one asked for, [dates] every snapshot of this chart. */
data class StoreChart(val title: String, val date: String, val dates: List<String>, val rows: List<ChartRow>)
data class StoreShot(val url: String, val width: Int, val height: Int)

/** The charts the website offers, as the API names them (slug to title). */
val CHART_TYPES = listOf("top-free" to "Top Free", "top-paid" to "Top Paid", "top-grossing" to "Top Grossing",
    "new-noteworthy" to "New & Noteworthy", "new-free" to "New Free", "new-paid" to "New Paid")
/** How the app lists can be ordered (the API's sort values). */
val CATEGORY_SORTS = listOf("versions" to "Most archived", "newest" to "Newest", "first_date" to "Oldest", "name" to "A to Z")

/** [status] is the HTTP status when the catalog answered with an error, else 0. */
class StoreError(message: String, val status: Int = 0) : Exception(message)

/** One archived copy of one version, as the version picker lists it. */
data class StoreCopy(val ipaId: Int, val version: String, val size: Long?, val minimumOS: String?)

/**
 * Legacy Store (legacystore.app) client, as the desktop app's crates/lighttouch-catalog/src/store.rs.
 * Nothing is downloaded until download() is called, and a copy is rechecked against the target
 * immediately before transfer.
 * MD5 checks archive integrity, not publisher authenticity.
 */
object LegacyStore {
    private const val BASE = "https://legacystore.app"
    private const val USER_AGENT = "LightTouchAndroid/0.1 (+https://github.com/blake-lucas/LightTouch)"
    private const val JSON_LIMIT = 8 shl 20
    private const val DOWNLOAD_LIMIT = 2L shl 30
    /** Archived copies whose executable is cut short (no device can load it), each with an intact copy of the same version. */
    private val BROKEN = mapOf(59853 to 179589)   // Fruit Ninja 1.7.4: the search row's copy
    /** The copy to select first for a search row that names [ipaId]. */
    fun preferred(ipaId: Int) = BROKEN[ipaId] ?: ipaId

    fun version(value: String?): List<Int>? = StoreVersions.parse(value)
    fun newer(a: List<Int>, b: List<Int>) = StoreVersions.newer(a, b)
    private fun requires(minimum: String?, target: StoreTarget): Boolean {
        if (minimum == null) return false
        val need = version(minimum) ?: throw StoreError("The minimum iOS version could not be verified.")
        return newer(need, version(target.ios) ?: throw StoreError("Invalid guest iOS version."))
    }

    /** Why a search row cannot be installed on the target, or null. Mirrors catalog.py's wording. */
    fun incompatibility(app: JSONObject): String? {
        val compat = app.optJSONObject("compat") ?: return null
        if (compat.optBoolean("compatible", true)) return null
        val reason = compat.optJSONArray("reasons")?.optString(0) ?: ""
        return when {
            reason == "armv6_slice_contains_armv7_code" || (reason.startsWith("no_") && reason.endsWith("_slice")) -> "Needs a newer processor"
            reason.startsWith("requires_ios_") -> "Requires iOS " + reason.removePrefix("requires_ios_")
            reason.startsWith("capability:!") || reason.endsWith("device_family") || reason == "family_not_requested" -> "Not made for this device"
            reason.startsWith("capability:") -> "Needs hardware this device does not have"
            reason == "encrypted" -> "Encrypted — cannot open in LightTouch"
            reason == "unavailable" -> "Download no longer available"
            reason == "not_analyzed" -> "Not checked for compatibility yet"
            else -> "Not compatible with this device"
        }
    }

    private fun open(url: String): HttpURLConnection {
        val parsed = URL(url)
        if (parsed.protocol != "https" || parsed.host.isNullOrEmpty() || parsed.userInfo != null) throw StoreError("The catalog supplied an invalid HTTPS address.")
        val connection = parsed.openConnection() as HttpsURLConnection
        connection.connectTimeout = 30_000; connection.readTimeout = 30_000
        connection.setRequestProperty("User-Agent", USER_AGENT)
        // HttpURLConnection never follows a redirect across protocols, so an https
        // request cannot be downgraded; the final URL is checked again below.
        val code = try { connection.responseCode } catch (e: java.net.UnknownHostException) {
            // GrapheneOS's Network permission and LineageOS's per-app network access both look like no DNS.
            throw StoreError("Could not reach Legacy Store. Check your connection, and that LightTouch is allowed network access in Android's app settings.")
        } catch (e: java.io.IOException) {
            throw StoreError("Could not connect to Legacy Store: ${e.message}")
        }
        if (connection.url.protocol != "https") { connection.disconnect(); throw StoreError("The catalog redirected to an insecure address.") }
        if (code != 200) {
            val body = connection.errorStream?.use { it.readNBytes(4096) }?.decodeToString() ?: ""
            connection.disconnect()
            throw StoreError(when {
                code == 400 && "device must be one of" in body -> "Legacy Store does not support this device yet."
                code == 503 -> "The Internet Archive is busy; try again later."
                else -> "Legacy Store could not answer the request (HTTP $code)."
            }, code)
        }
        return connection
    }

    private fun url(path: String, query: Map<String, String>) = BASE + path + if (query.isEmpty()) "" else "?" + query.entries.joinToString("&") {
        URLEncoder.encode(it.key, "UTF-8") + "=" + URLEncoder.encode(it.value, "UTF-8")
    }

    /** Browsing answers, kept for ten minutes: the catalog asks to be cached, and tabs get revisited. */
    private val responses = LruCache<String, Pair<Long, JSONObject>>(64)
    private const val CACHE_MS = 10 * 60_000L

    /** [cached] is for browsing only: what is checked right before a download is always asked for afresh. */
    private fun json(path: String, query: Map<String, String> = emptyMap(), cached: Boolean = false): JSONObject = fetch(url(path, query), cached)

    private fun fetch(url: String, cached: Boolean): JSONObject {
        if (cached) responses.get(url)?.let { (time, value) -> if (System.currentTimeMillis() - time < CACHE_MS) return value }
        val connection = open(url)
        val body = try { connection.inputStream.use { it.readNBytes(JSON_LIMIT + 1) } } finally { connection.disconnect() }
        if (body.size > JSON_LIMIT) throw StoreError("The catalog response exceeds the size limit.")
        val value = try { JSONObject(body.decodeToString()) } catch (_: org.json.JSONException) { throw StoreError("Legacy Store sent an unreadable response.") }
        if (cached) responses.put(url, System.currentTimeMillis() to value)
        return value
    }

    private fun apps(query: Map<String, String>, target: StoreTarget): List<JSONObject> {
        val list = json("/api/emulator/apps", query + mapOf("device" to target.productType, "os" to target.ios)).optJSONArray("apps")
            ?: throw StoreError("Legacy Store sent an unreadable app list.")
        if (list.length() > 500) throw StoreError("Legacy Store sent an unreadable app list.")
        return (0 until list.length()).map { index ->
            val app = list.optJSONObject(index)
            if (app == null || app.optInt("ipa_id") <= 0 || app.optString("name").isEmpty() || app.optString("download_url").isEmpty())
                throw StoreError("Legacy Store sent an unreadable app entry.")
            app
        }
    }

    /** Empty query lists the catalog's featured compatible apps; a query includes incompatible rows. */
    fun search(query: String, target: StoreTarget): List<JSONObject> =
        apps(if (query.isBlank()) mapOf("limit" to "50") else mapOf("limit" to "50", "q" to query.trim(), "incompatible" to "include"), target)

    fun compatibleCopy(ipaId: Int, target: StoreTarget): JSONObject {
        val found = apps(mapOf("ipa_id" to ipaId.toString()), target)
        if (found.size != 1 || found[0].optInt("ipa_id") != ipaId) throw StoreError("This copy is no longer available.")
        incompatibility(found[0])?.let { throw StoreError(it) }
        return found[0]
    }

    fun copyDetails(ipaId: Int): JSONObject {
        val copy = json("/api/v1/copies/$ipaId")
        if (copy.optString("ipa_id") != ipaId.toString()) throw StoreError("Legacy Store returned a different archived copy.")
        return copy
    }

    private fun runs(architectures: JSONArray?, target: StoreTarget): Boolean =
        architectures != null && (0 until architectures.length()).map { architectures.optString(it) }
            .any { it == target.arch || (target.arch == "armv7" && it == "armv6") }

    /** How the catalog addresses an app: its App Store id, or its bundle id (which the catalog redirects), URL-escaped for a path. */
    fun appKey(app: JSONObject): String? =
        (app.text("app_store_id") ?: app.text("bundle_id") ?: app.text("app_url")?.trimEnd('/')?.substringAfterLast('/'))
            ?.let { URLEncoder.encode(it, "UTF-8").replace("+", "%20") }

    /**
     * Every archived copy of the app that could run on the target, newest first. The
     * copy the search row named is always kept so the picker can show it selected, unless it is broken.
     */
    fun versions(app: JSONObject, target: StoreTarget): List<StoreCopy> {
        val key = appKey(app) ?: throw StoreError("This app has no catalog identifier.")
        val data = json("/api/v1/apps/$key/versions", cached = true).optJSONArray("data")
            ?: throw StoreError("Legacy Store sent unreadable version information.")
        return runnableCopies(data, app.optInt("ipa_id"), target)
    }

    /** The copies in a catalog version list that run on [target], plus the one named [chosen] (if any) whether it runs or not. */
    private fun runnableCopies(data: JSONArray, chosen: Int, target: StoreTarget): List<StoreCopy> {
        val rows = mutableListOf<StoreCopy>()
        for (v in 0 until data.length()) {
            val entry = data.optJSONObject(v) ?: continue
            val minimum = entry.optString("minimum_os_version").ifEmpty { null }
            val copies = entry.optJSONArray("copies") ?: continue
            for (c in 0 until copies.length()) {
                val copy = copies.optJSONObject(c) ?: continue
                val id = copy.optString("ipa_id").toIntOrNull()?.takeIf { it !in BROKEN } ?: continue
                val machoMinimum = copy.optString("macho_min_os").ifEmpty { null }
                val fits = copy.optString("install_status") == "installable" && runs(copy.optJSONArray("architectures"), target) &&
                    runCatching { !requires(minimum, target) && !requires(machoMinimum, target) }.getOrDefault(false)
                if (fits || id == chosen) rows += StoreCopy(id, entry.optString("version", "Unknown"),
                    copy.optLong("size").takeIf { it > 0 }, listOfNotNull(minimum, machoMinimum).maxWithOrNull { a, b ->
                        val x = version(a) ?: listOf(0, 0, 0); val y = version(b) ?: listOf(0, 0, 0)
                        if (newer(x, y)) 1 else if (newer(y, x)) -1 else 0 })
            }
        }
        return rows
    }

    /** An installed app the catalog knows: a listing row for its page, and the newest version that runs on the device. */
    class Known(val app: JSONObject, val newest: String?)

    /**
     * The catalog's entry for a bundle id (the catalog redirects the bundle id to the app), or null when it has
     * none. One cached request: the app's own answer lists its versions. Another identity coming back is "none".
     */
    fun lookup(bundleId: String, target: StoreTarget): Known? {
        val body = try { json("/api/v1/apps/" + URLEncoder.encode(bundleId, "UTF-8").replace("+", "%20"), cached = true) }
            catch (e: StoreError) { if (e.status == 404) return null else throw e }
        if (!body.optString("bundle_id").equals(bundleId, ignoreCase = true)) return null
        val app = JSONObject().put("bundle_id", body.optString("bundle_id")).put("name", body.text("name") ?: body.text("display_name") ?: bundleId)
        for (key in listOf("app_store_id", "developer", "icon_url")) body.text(key)?.let { app.put(key, it) }
        val versions = body.optJSONArray("versions")?.let { runnableCopies(it, 0, target) } ?: emptyList()
        return Known(app, StoreVersions.newest(versions.map { it.version }))
    }

    /** catalog.py check_copy: identity, availability, slice, family and minimum OS. */
    fun checkCopy(copy: JSONObject, app: JSONObject, target: StoreTarget) {
        if (copy.optString("ipa_id") != app.optInt("ipa_id").toString() || copy.optString("bundle_id") != app.optString("bundle_id"))
            throw StoreError("The archived copy no longer matches this app.")
        if (!copy.optBoolean("available", false)) throw StoreError("This archived download is no longer available.")
        val binary = copy.optJSONObject("binary") ?: throw StoreError("This copy has not been analyzed for compatibility.")
        if (binary.optString("install_status") != "installable") throw StoreError("This copy is encrypted or has not been classified as installable.")
        if (!runs(binary.optJSONArray("architectures"), target) || (target.arch == "armv6" && binary.optBoolean("armv7_code", false)))
            throw StoreError("This copy needs a newer processor than this device has.")
        binary.optJSONArray("device_family_macho")?.let { families ->
            if (families.length() > 0 && (0 until families.length()).none { families.optString(it) == target.family })
                throw StoreError("This copy is not made for this device.")
        }
        for (minimum in listOf(app.optString("min_os"), binary.optString("macho_min_os")).filter { it.isNotEmpty() && it != "null" })
            if (requires(minimum, target)) throw StoreError("Requires iOS $minimum; this device runs iOS ${target.ios}.")
    }

    /** Download one revalidated copy into a fresh private directory; the caller deletes it after install. */
    fun download(app: JSONObject, target: StoreTarget, scratch: File, progress: (Long, Long?) -> Unit, cancelled: () -> Boolean): File {
        val current = compatibleCopy(app.optInt("ipa_id"), target)
        if (current.optString("bundle_id") != app.optString("bundle_id")) throw StoreError("The archived copy no longer matches this app.")
        val details = copyDetails(current.optInt("ipa_id"))
        checkCopy(details, current, target)
        val checksum = details.optString("md5").takeIf { it.isNotEmpty() && it != "null" }
        if (checksum != null && !Regex("[0-9a-fA-F]{32}").matches(checksum)) throw StoreError("The archive supplied an invalid file checksum.")
        val size = if (details.isNull("size")) null else details.optLong("size").also {
            if (it !in 1..DOWNLOAD_LIMIT) throw StoreError("The archive supplied an invalid file size.")
        }
        val directory = File(scratch, "catalog-${current.optInt("ipa_id")}-${System.nanoTime()}")
        if (!directory.mkdirs()) throw StoreError("Could not create a download folder.")
        val partial = File(directory, "download.partial"); val final = File(directory, "App.ipa")
        try {
            val connection = open(current.optString("download_url"))
            val digest = MessageDigest.getInstance("MD5")
            var count = 0L
            try {
                connection.inputStream.use { input: InputStream -> partial.outputStream().use { output ->
                    val buffer = ByteArray(1 shl 16)
                    while (true) {
                        if (cancelled()) throw StoreError("Download cancelled.")
                        val read = input.read(buffer)
                        if (read < 0) break
                        count += read
                        if (count > DOWNLOAD_LIMIT || (size != null && count > size)) throw StoreError("The download exceeds its expected size.")
                        output.write(buffer, 0, read); digest.update(buffer, 0, read)
                        progress(count, size)
                    }
                    output.fd.sync()
                } }
            } finally { connection.disconnect() }
            if (count == 0L || (size != null && count != size)) throw StoreError("The download is incomplete or differs from the archive size.")
            if (checksum != null && digest.digest().joinToString("") { "%02x".format(it) } != checksum.lowercase())
                throw StoreError("The downloaded IPA failed its checksum check.")
            if (!partial.renameTo(final)) throw StoreError("Could not publish the download.")
            return final
        } catch (e: Exception) {
            directory.deleteRecursively()
            throw if (e is StoreError) e else StoreError("Download failed: ${e.message}")
        }
    }

    private fun rows(list: org.json.JSONArray?, what: String): List<JSONObject> {
        if (list == null || list.length() > 500) throw StoreError("Legacy Store sent an unreadable $what.")
        return (0 until list.length()).map { index ->
            val app = list.optJSONObject(index)
            if (app == null || app.optString("name").isEmpty()) throw StoreError("Legacy Store sent an unreadable app entry.")
            app
        }
    }

    /** The categories that have apps, A to Z. */
    fun genres(): List<StoreGenre> {
        val data = json("/api/v1/genres", cached = true).optJSONArray("data") ?: throw StoreError("Legacy Store sent an unreadable category list.")
        return (0 until data.length()).mapNotNull { data.optJSONObject(it) }
            .map { StoreGenre(it.optString("genre_id"), it.optString("name"), it.optInt("app_count")) }
            .filter { it.id.isNotEmpty() && it.name.isNotEmpty() && it.count > 0 }.sortedBy { it.name.lowercase() }
    }

    /** The first page of a category's apps, in [CATEGORY_SORTS] order; [page] follows [StorePage.next]. */
    fun genreApps(genreId: String, sort: String): StorePage =
        page(url("/api/v1/apps", mapOf("genre" to genreId, "sort" to sort, "limit" to "50")))

    /** A page of a paged listing, from the address the catalog gave for it. Only the catalog's own addresses are followed. */
    fun page(address: String): StorePage {
        if (URL(address).host != URL(BASE).host) throw StoreError("The catalog supplied an unexpected address.")
        val body = fetch(address, true)
        return StorePage(rows(body.optJSONArray("data"), "app list"), body.optInt("total"), body.text("next_url"))
    }

    fun collections(): List<StoreCollection> {
        val data = json("/api/v1/collections", cached = true).optJSONArray("data") ?: throw StoreError("Legacy Store sent an unreadable collection list.")
        return (0 until data.length()).mapNotNull { data.optJSONObject(it) }
            .map { StoreCollection(it.optString("slug"), it.optString("title"), it.text("subtitle") ?: it.text("series"), it.optInt("app_count")) }
            .filter { it.slug.isNotEmpty() && it.title.isNotEmpty() }
    }

    /** A collection's apps in the editors' order; a member's note, if it has one, rides along as "label". */
    fun collection(slug: String): StorePage {
        val members = json("/api/v1/collections/" + URLEncoder.encode(slug, "UTF-8"), cached = true).optJSONArray("members")
            ?: throw StoreError("Legacy Store sent an unreadable collection.")
        val apps = (0 until members.length()).mapNotNull { members.optJSONObject(it) }.mapNotNull { member ->
            // A member the archive has not preserved has only its label, and is listed greyed out.
            member.optJSONObject("app")?.also { app -> member.text("label")?.let { app.put("label", it) } }
                ?: member.text("label")?.let { JSONObject().put("name", it).put("ghost", true) }
        }
        return StorePage(rows(org.json.JSONArray(apps), "collection"), apps.size, null)
    }

    /** One archived chart. [date] (YYYY-MM-DD) picks the snapshot nearest it; the newest when null. */
    fun chart(type: String, device: String, genreId: String?, date: String?): StoreChart {
        val query = mutableMapOf("type" to type, "device" to device)
        genreId?.let { query["genre"] = it }; date?.let { query["date"] = it }
        val body = try { json("/api/v1/charts", query, cached = true) } catch (e: StoreError) {
            // Most type, category and device combinations were never captured.
            throw if (e.status == 404) StoreError("No chart was archived for this selection.", 404) else e
        }
        val positions = body.optJSONArray("positions") ?: throw StoreError("Legacy Store sent an unreadable chart.")
        val dates = body.optJSONArray("available_dates")?.let { list -> (0 until list.length()).map { list.optString(it) } } ?: emptyList()
        return StoreChart(body.optJSONObject("chart_type")?.optString("name") ?: type, body.optString("snapshot_date"), dates,
            (0 until positions.length()).mapNotNull { positions.optJSONObject(it) }.map {
                ChartRow(it.optInt("position"), it.optString("name"), it.text("developer"), it.optJSONObject("app"))
            })
    }

    /** What the catalog says about an app beyond its listing row: category, first release, copyright. */
    fun appInfo(app: JSONObject): JSONObject {
        val key = appKey(app) ?: throw StoreError("This app has no catalog identifier.")
        return json("/api/v1/apps/$key", cached = true)
    }

    /** The newest archived set of screenshots (the catalog has no app descriptions). */
    fun screenshots(app: JSONObject): List<StoreShot> {
        val key = appKey(app) ?: return emptyList()
        val sets = json("/api/v1/apps/$key/screenshots", cached = true).optJSONArray("data") ?: return emptyList()
        val shots = (0 until sets.length()).mapNotNull { sets.optJSONObject(it)?.optJSONArray("shots") }.firstOrNull { it.length() > 0 } ?: return emptyList()
        return (0 until shots.length()).mapNotNull { shots.optJSONObject(it) }
            .filter { it.optString("url").isNotEmpty() }.map { StoreShot(it.optString("url"), it.optInt("width"), it.optInt("height")) }
    }

    private val screens = LruCache<String, Bitmap>(24)
    /** A screenshot at about 400 px wide, which is plenty for a strip; null when it can't be had. */
    fun screenshot(url: String): Bitmap? {
        screens.get(url)?.let { return it }
        return runCatching {
            val connection = open(url)
            val bytes = try { connection.inputStream.use { it.readNBytes(JSON_LIMIT) } } finally { connection.disconnect() }
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }.also { BitmapFactory.decodeByteArray(bytes, 0, bytes.size, it) }
            val options = BitmapFactory.Options().apply { inSampleSize = Integer.highestOneBit((bounds.outWidth / 400).coerceAtLeast(1)) }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, options)
        }.getOrNull()?.also { screens.put(url, it) }
    }

    private val icons = LruCache<String, Bitmap>(128)
    /** Small app icons for the result list; failures just leave the row without one. */
    fun icon(url: String): Bitmap? {
        icons.get(url)?.let { return it }
        return runCatching {
            val connection = open(url)
            try { connection.inputStream.use { BitmapFactory.decodeStream(it) } } finally { connection.disconnect() }
        }.getOrNull()?.also { icons.put(url, it) }
    }
}
