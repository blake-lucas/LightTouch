package org.lighttouch.android

/**
 * The one gate every Legacy Store request passes, so the app stays a polite client of a small volunteer-run service
 * (it answers bursts with 429). Same rules as the desktop's crates/lighttouch-catalog/src/throttle.rs: at most 4
 * requests in flight, 4 a second with bursts of 8, and a 429 (or a 503 that says when to come back) pauses every
 * request for as long as the server asks. No Android in it; times are System.nanoTime() values.
 */
class StoreThrottle(private val clock: () -> Long = System::nanoTime) {
    companion object {
        const val MAX_IN_FLIGHT = 4
        const val INTERVAL_NS = 250_000_000L
        const val BURST = 8
        /** A 429 without a usable Retry-After. */
        const val DEFAULT_PAUSE_NS = 30_000_000_000L
        const val MAX_PAUSE_NS = 600_000_000_000L

        /** What the person reads during a pause. */
        fun slowDown(leftNs: Long) =
            "Legacy Store asked LightTouch to slow down. Try again in ${((leftNs + 999_999_999) / 1_000_000_000).coerceAtLeast(1)} seconds."
    }

    class Paused(val leftNs: Long) : Exception(slowDown(leftNs))

    private val lock = Object()
    private var inFlight = 0
    /** When the next request would start if requests went out exactly one interval apart (GCRA). */
    private var next: Long? = null
    private var pausedUntil: Long? = null

    /** How long a request starting at [now] waits for its turn; reserves that turn. */
    fun reserve(now: Long): Long = synchronized(lock) {
        val at = next?.let { if (it - now > 0) it else now } ?: now
        next = at + INTERVAL_NS
        ((at - now) - INTERVAL_NS * (BURST - 1)).coerceAtLeast(0)
    }

    fun paused(now: Long): Long? = synchronized(lock) { pausedUntil?.let { it - now }?.takeIf { it > 0 } }

    fun pause(now: Long, retryAfterNs: Long?) = synchronized(lock) {
        val until = now + (retryAfterNs ?: DEFAULT_PAUSE_NS).coerceIn(1_000_000_000L, MAX_PAUSE_NS)
        pausedUntil = pausedUntil?.let { if (it - until > 0) it else until } ?: until
    }

    /**
     * Wait for a turn to send one request; call [leave] when its answer has arrived. [patient] requests (icons,
     * screenshots, loaded in the background) also wait out a pause; the others throw [Paused] at once, so the person
     * sees why instead of a stalled screen.
     */
    fun enter(patient: Boolean) {
        val delay = synchronized(lock) {
            while (true) {
                val left = paused(clock())
                if (left != null) { if (!patient) throw Paused(left); waitNs(left) }
                else if (inFlight >= MAX_IN_FLIGHT) lock.wait()
                else break
            }
            inFlight++
            reserve(clock())
        }
        try {
            if (delay > 0) Thread.sleep(delay / 1_000_000, (delay % 1_000_000).toInt())
            // A pause that began while this one waited for its turn still applies.
            synchronized(lock) {
                while (true) {
                    val left = paused(clock()) ?: break
                    if (!patient) throw Paused(left)
                    waitNs(left)
                }
            }
        } catch (e: Throwable) { leave(); throw e }
    }

    fun leave() = synchronized(lock) { inFlight--; lock.notifyAll() }

    /** Note an answer's status: a 429, or a 503 with Retry-After, pauses every request. Returns the pause left, if any. */
    fun answered(status: Int, retryAfter: String?): Long? {
        if (status != 429 && !(status == 503 && retryAfter != null)) return null
        pause(clock(), retryAfter?.trim()?.toLongOrNull()?.takeIf { it >= 0 }?.let { it * 1_000_000_000L })
        return paused(clock())
    }

    private fun waitNs(ns: Long) = lock.wait((ns / 1_000_000).coerceAtLeast(1))
}
