package org.lighttouch.inferno

import org.xmlpull.v1.XmlPullParser
import org.xmlpull.v1.XmlPullParserFactory
import java.io.File
import java.io.InputStream

/**
 * Just enough of an XML property list reader for `BuildManifest.plist`:
 * dict, array, string, integer, real, true/false, data and date. Binary plists
 * are not read; an IPSW's manifest is XML.
 */
object Plist {
    fun parse(input: InputStream): Any? {
        val parser = XmlPullParserFactory.newInstance().newPullParser()
        parser.setFeature(XmlPullParser.FEATURE_PROCESS_NAMESPACES, false)
        parser.setInput(input, null)
        while (parser.next() != XmlPullParser.END_DOCUMENT) {
            if (parser.eventType == XmlPullParser.START_TAG && parser.name != "plist") return value(parser)
        }
        return null
    }

    fun parse(file: File): Any? = runCatching { file.inputStream().buffered().use { parse(it) } }.getOrNull()

    /** Reads the element the parser is on and leaves it on that element's end tag. */
    private fun value(p: XmlPullParser): Any? = when (p.name) {
        "dict" -> {
            val map = LinkedHashMap<String, Any?>()
            var key: String? = null
            while (p.next() != XmlPullParser.END_TAG) {
                if (p.eventType != XmlPullParser.START_TAG) continue
                if (p.name == "key") key = p.nextText() else { map[key ?: ""] = value(p); key = null }
            }
            map
        }
        "array" -> {
            val list = ArrayList<Any?>()
            while (p.next() != XmlPullParser.END_TAG) {
                if (p.eventType == XmlPullParser.START_TAG) list += value(p)
            }
            list
        }
        "string", "date" -> p.nextText()
        "data" -> p.nextText().trim()
        "integer" -> p.nextText().trim().toLongOrNull()
        "real" -> p.nextText().trim().toDoubleOrNull()
        "true" -> { p.next(); true }
        "false" -> { p.next(); false }
        else -> { skip(p); null }
    }

    private fun skip(p: XmlPullParser) {
        var depth = 1
        while (depth > 0) {
            when (p.next()) {
                XmlPullParser.START_TAG -> depth++
                XmlPullParser.END_TAG -> depth--
                XmlPullParser.END_DOCUMENT -> return
            }
        }
    }
}
