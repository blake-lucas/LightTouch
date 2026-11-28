package org.lighttouch.android

import android.os.Bundle

/** A library record as the UI shows it. */
data class DeviceSummary(
    val id: String, val name: String, val board: String, val build: String,
    val selected: Boolean, val dirty: Boolean, val contacts: String, val accelerometer: Boolean, val gyroscope: Boolean,
    val target: StoreTarget?,
    /** The board has a cellular modem (iPhone 3GS, iPhone 4): it takes carrier settings. */
    val cellular: Boolean = false,
) {
    /** "iPod touch 2G" from the catalog, else the board. */
    val model get() = target?.let { shortProductName(it.productType) } ?: board
    /** "iPod touch 2G · iOS 3.1.3", or the board and build when the catalog does not know them. */
    val subtitle get() = target?.let { "${shortProductName(it.productType)} · iOS ${it.ios}" } ?: "$board · $build"

    companion object {
        fun of(record: Bundle, targets: Map<String, StoreTarget>) = DeviceSummary(
            record.getString("id") ?: "", record.getString("name") ?: "", record.getString("board") ?: "", record.getString("build") ?: "",
            record.getBoolean("selected"), record.getBoolean("dirty"), record.getString("contacts") ?: "1",
            record.getBoolean("acceleration"), record.getBoolean("gyro"),
            targets["${record.getString("board")}-${record.getString("build")}"], record.getBoolean("cellular"))
    }
}

/** This list with the item at [from] moved to [to] (clamped to the ends), the rest in their order: the devices sheet's
 *  drag to reorder. Unchanged when [from] is out of range. */
internal fun <T> List<T>.moved(from: Int, to: Int): List<T> {
    if (from !in indices) return this
    val list = toMutableList()
    list.add(to.coerceIn(0, list.size - 1), list.removeAt(from))
    return list
}

internal fun shortProductName(productType: String): String = when (productType) {
    "iPod1,1" -> "iPod touch"; "iPod2,1" -> "iPod touch 2G"; "iPod3,1" -> "iPod touch 3G"; "iPod4,1" -> "iPod touch 4G"
    "iPhone1,1" -> "iPhone"; "iPhone1,2" -> "iPhone 3G"; "iPhone2,1" -> "iPhone 3GS"; "iPhone3,1" -> "iPhone 4"
    "iPhone5,2" -> "iPhone 5"; "iPad1,1" -> "iPad"; else -> productType
}

/** Whether a name being typed fits: 60 characters, and the library's 127-byte UTF-8 limit (an emoji or CJK name reaches it first). */
internal fun fitsDeviceName(name: String) = name.length <= 60 && name.trim().toByteArray(Charsets.UTF_8).size <= 127

/** A name LightTouch made up from the firmware id rather than one the user chose. */
internal fun isGeneratedDeviceName(name: String, board: String, build: String) =
    name.isBlank() || name == "$board $build" || name.startsWith("$board-$build") && name.endsWith("prepared")

/** Default name for a new device: "<model> <iOS version>", numbered if another device has it. */
internal fun defaultDeviceName(board: String, build: String, targets: Map<String, StoreTarget>, taken: Collection<String>): String {
    val base = targets["$board-$build"]?.let { "${shortProductName(it.productType)} ${it.ios}" } ?: "$board $build"
    if (base !in taken) return base
    return generateSequence(2) { it + 1 }.map { "$base ($it)" }.first { it !in taken }
}
