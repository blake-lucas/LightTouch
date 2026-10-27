package org.lighttouch.android

/** Retry admission contention, never an accepted mutation. Independent of Android
 * so boot readiness, competing requests and lost completions can be exercised. */
internal class GuestNetworkOperation(
    private val request: (Int, String, Int) -> Array<String>,
    private val poll: (Long) -> Array<String>,
    private val cancel: (Long) -> Unit,
    private val active: () -> Boolean,
    private val now: () -> Long,
    private val sleep: (Long) -> Unit
) {
    fun execute(kind: Int, argument: String = "", admissionWaitMs: Long = 20_000): String {
        val admissionDeadline = now() + admissionWaitMs
        val id: Long
        while (true) {
            check(active()) { "Network setup stopped" }
            val admitted = request(kind, argument, 20_000)
            val candidate = admitted[0].toLong()
            if (candidate != 0L) { id = candidate; break }
            // LT_BUSY and LT_AGAIN explicitly mean this request was not sent.
            // Invalid/unsupported operations fail immediately, without retries.
            check(admitted[2].toInt() in listOf(3, 8) && now() < admissionDeadline) {
                admitted[1].ifEmpty { "Guest tools did not become available" }
            }
            sleep(100)
        }
        try {
            val completionDeadline = now() + 25_000
            while (active()) {
                val reply = poll(id)
                check(reply[3].isEmpty()) { reply[3] }
                when (reply[0].toInt()) {
                    1 -> {
                        check(now() < completionDeadline) { "Guest network setup timed out; restart to try again" }
                        sleep(100)
                    }
                    2 -> {
                        check(reply[1] == "0") { "Guest network setup step $kind failed (${reply[1]})" }
                        return reply[2]
                    }
                    else -> error("Guest network setup step $kind failed (${reply[1]}). Update guest tools and restart.")
                }
            }
            error("Network setup stopped")
        } finally {
            cancel(id)
        }
    }
}
