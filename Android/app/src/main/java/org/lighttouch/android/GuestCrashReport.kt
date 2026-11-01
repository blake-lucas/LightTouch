package org.lighttouch.android

/**
 * One iOS crash report, read for the part that explains the crash.
 *
 * iOS writes these itself (com.apple.crashreportcopymobile) and they are the only place that names the library
 * a crash happened in. The store's "Last crash" used to keep only the header fields, so every report looked like
 * "EXC_BAD_ACCESS at 0x0" and nothing more -- the frames, which are the answer, were filtered out. This keeps
 * the faulting thread's stack, the image it faulted in and its path, and says whose side the crash is on.
 */
internal object GuestCrashReport {
    /** Frames of the faulting thread kept in the text; a report can list hundreds, the first ones name the crash. */
    private const val FRAMES = 24
    private const val TRACE_FRAMES = 12

    private val FRAME = Regex("""^\s*\d+\s+(\S+)\s+0x[0-9a-fA-F]+""")

    /** A verdict sentence, and the report text the verdict was read from. */
    data class Summary(val verdict: String, val body: String)

    /** Runtime stubs the app calls into; a crash in one of them is really about the frame that called it. */
    private fun stub(image: String) = image.startsWith("libobjc") || image == "dyld" ||
        image.startsWith("libsystem_kernel") || image.startsWith("libsystem_c") || image.startsWith("libsystem_malloc") ||
        image.startsWith("libSystem")

    fun summarize(name: String, raw: String): Summary {
        val lines = raw.replace("\r\n", "\n").replace('\r', '\n').lines()
        val process = field(lines, "Process")?.substringBefore(" [")?.trim().orEmpty()
        val guest = field(lines, "OS Version")?.removePrefix("iOS")?.trim()?.substringBefore(" (")?.trim().orEmpty()
        val frames = crashedFrames(lines)
        val images = frames.mapNotNull { FRAME.find(it)?.groupValues?.get(1) }
        val image = images.firstOrNull()
        // The frame that called into the runtime stub is the one that made the mistake.
        val caller = images.firstOrNull { !stub(it) }?.takeIf { it != image }
        val imagePath = binaryImagePath(lines, image)
        val callerPath = binaryImagePath(lines, caller)
        val dyld = block(lines, "Dyld Error Message:", 4)
        val codes = listOfNotNull(field(lines, "Exception Codes"), field(lines, "Exception Subtype")).joinToString(" ")
        val termination = field(lines, "Termination Reason").orEmpty()
        val verdict = verdict(process, guest, image, imagePath, caller, callerPath, dyld, codes, termination)

        val body = buildString {
            if (name.isNotEmpty()) append(name).append('\n')
            for (key in listOf("Process", "Date/Time", "OS Version", "Exception Type", "Exception Codes",
                               "Exception Subtype", "Crashed Thread", "Termination Reason")) {
                field(lines, key)?.takeIf { it.isNotEmpty() }?.let { append(key).append(": ").append(it).append('\n') }
            }
            val specific = block(lines, "Application Specific Information:", 6)
            if (specific.isNotEmpty()) {
                append("Application Specific Information:\n")
                specific.forEach { append(it.trim()).append('\n') }
            }
            if (image != null) append("\nFaulted in: ").append(image).append(imagePath?.let { "  ($it)" } ?: "").append('\n')
            if (caller != null) append("Called from: ").append(caller).append(callerPath?.let { "  ($it)" } ?: "").append('\n')
            val exceptionTrace = block(lines, "Last Exception Backtrace:", TRACE_FRAMES)
            if (exceptionTrace.isNotEmpty()) {
                append("\nLast Exception Backtrace:\n")
                exceptionTrace.forEach { append(it.trim()).append('\n') }
            }
            if (frames.isNotEmpty()) {
                append("\n").append(threadHeader(lines)).append('\n')
                frames.forEach { append(it.trim()).append('\n') }
            }
            if (dyld.isNotEmpty()) {
                append("\nDyld Error Message:\n")
                dyld.forEach { append(it.trim()).append('\n') }
            }
        }.trimEnd()
        return Summary(verdict, body)
    }

    /** The frames of the thread iOS marked as crashed; the one the `Crashed Thread:` number names otherwise. */
    private fun crashedFrames(lines: List<String>): List<String> {
        val marked = lines.indexOfFirst { it.trimStart().startsWith("Thread ") && it.contains(" Crashed:") }
        val index = field(lines, "Crashed Thread")?.substringBefore(' ')?.toIntOrNull()
        val start = marked.takeIf { it >= 0 }
            ?: index?.let { wanted -> lines.indexOfFirst { it.trimStart().startsWith("Thread $wanted:") } }?.takeIf { it >= 0 }
            ?: return emptyList()
        return framesAfter(lines, start).take(FRAMES)
    }

    private fun threadHeader(lines: List<String>): String {
        val marked = lines.firstOrNull { it.trimStart().startsWith("Thread ") && it.contains(" Crashed:") }
        if (marked != null) return marked.trim()
        val index = field(lines, "Crashed Thread")?.substringBefore(' ')?.toIntOrNull()
        return lines.firstOrNull { index != null && it.trimStart().startsWith("Thread $index:") }?.trim() ?: "Thread"
    }

    /** The frames that follow a thread header: numbered lines, up to the thread state or the next section. */
    private fun framesAfter(lines: List<String>, header: Int): List<String> {
        val out = mutableListOf<String>()
        for (i in header + 1 until lines.size) {
            val line = lines[i]
            if (line.isBlank()) continue
            if (!line.trimStart()[0].isDigit()) break
            out += line
        }
        return out
    }

    /** The `Binary Images:` line for [image]: its slide and full path, which is how a system lib is recognised. */
    private fun binaryImagePath(lines: List<String>, image: String?): String? {
        if (image == null) return null
        val start = lines.indexOfFirst { it.trim() == "Binary Images:" }
        if (start < 0) return null
        for (i in start + 1 until lines.size) {
            val line = lines[i].trim()
            if (line.isEmpty()) continue
            val tokens = line.split(Regex("\\s+"))
            val at = tokens.indexOfFirst { it == image || it == "+$image" }
            if (at < 0) continue
            return tokens.lastOrNull()?.takeIf { it.startsWith("/") }
        }
        return null
    }

    /** The first header line that starts with [key], with the lines that follow it, up to [count]. */
    private fun block(lines: List<String>, key: String, count: Int): List<String> {
        val start = lines.indexOfFirst { it.trim() == key || it.trimStart().startsWith(key) }
        if (start < 0) return emptyList()
        val first = lines[start].substringAfter(key).trim()
        val out = mutableListOf<String>()
        if (first.isNotEmpty()) out += first
        for (i in start + 1 until lines.size) {
            if (out.size >= count) break
            val line = lines[i]
            // These blocks end at a blank line (the next section), never in the middle of their own lines.
            if (line.isBlank()) break
            out += line
        }
        return out
    }

    private fun field(lines: List<String>, key: String): String? =
        lines.firstOrNull { it.startsWith("$key:") }?.substringAfter(':')?.trim()

    /**
     * Whose side the crash is on. The frame that faulted names a binary, and that binary's path says whether it is
     * the app or the emulated system; dyld's message and the exception codes say what kind of failure it was.
     */
    private fun verdict(process: String, guest: String, image: String?, path: String?, caller: String?, callerPath: String?,
                        dyld: List<String>, codes: String, termination: String): String {
        val message = dyld.joinToString(" ")
        val symbol = Regex("""Symbol not found:\s*(\S+)""").find(message)?.groupValues?.get(1)
        val expected = Regex("""Expected in:\s*(\S+)""").find(message)?.groupValues?.get(1)
        if (symbol != null) {
            return "The app needs $symbol, which ${expected ?: "this iOS"} doesn't have. It was built for a newer iOS " +
                "than ${if (guest.isEmpty()) "this one" else "iOS $guest"}."
        }
        val library = Regex("""Library not loaded:\s*(\S+)""").find(message)?.groupValues?.get(1)
        if (library != null) return "The app needs $library, which this iOS doesn't include."
        if (termination.contains("CODESIGN", ignoreCase = true) || message.contains("code signature", ignoreCase = true)) {
            return "iOS rejected the app's code signature and closed it. This is not an emulation problem."
        }
        if (codes.contains("0x8badf00d", ignoreCase = true) || message.contains("0x8badf00d")) {
            return "iOS closed the app because it took too long to start. The emulator was too slow; the app itself is probably fine."
        }
        val nullPointer = codes.contains("at 0x0") || codes.contains("at 0x00000000") || codes.contains("KERN_INVALID_ADDRESS") ||
            codes.contains("KERN_PROTECTION_FAILURE")
        val system = systemSide(process, image, path, caller, callerPath)
        return when {
            image == null -> "The crash report doesn't say where the app crashed."
            nullPointer && image != null && stub(image) && caller != null ->
                "The app crashed in $image, called from $caller, using something that didn't exist. On a real " +
                    "device that is a bug in the app; here it can also mean the emulated iOS didn't provide " +
                    "something the app expected. The system log below may say which."
            system -> "The app crashed inside ${caller ?: image}, part of iOS. The emulation is the likely cause, not " +
                "the app."
            else -> "The app crashed in its own code (${caller ?: image}). The emulation can still be the cause if the app " +
                "relied on something LightTouch doesn't support yet."
        }
    }

    /** Whether the crash site, or rather the frame that failed, is a library of the emulated system. */
    private fun systemSide(process: String, image: String?, path: String?, caller: String?, callerPath: String?): Boolean {
        if (caller != null && callerPath != null) return callerPath.startsWith("/System/") || callerPath.startsWith("/usr/lib/")
        val target = caller ?: image ?: return false
        val targetPath = if (target == caller) callerPath else path
        if (targetPath != null) return targetPath.startsWith("/System/") || targetPath.startsWith("/usr/lib/")
        return target.endsWith(".dylib") || target != process
    }
}
