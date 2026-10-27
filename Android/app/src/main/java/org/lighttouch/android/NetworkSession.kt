package org.lighttouch.android

/** Saved preferences and the running worker's applied configuration are distinct. Every change applies live,
 * turning networking on included: the engine forwards to the proxy socket from boot and the worker makes the
 * proxy the first time networking is on. Retry only policy admission while the engine is starting, never guest
 * mutations. */
internal class NetworkSession {
    data class Snapshot(val active: NetworkConfiguration, val notice: String)
    private var active = NetworkConfiguration()
    private var requested = active
    private var pending = false
    private var notice = ""

    @Synchronized fun begin(config: NetworkConfiguration) {
        active = config
        requested = config
        pending = false
        notice = ""
    }

    @Synchronized fun request(config: NetworkConfiguration) {
        config.validate()
        requested = config
        pending = requested != active
        notice = if (pending) "Applying network settings…" else ""
    }

    @Synchronized fun refresh(apply: (NetworkConfiguration) -> Pair<Int, String>) {
        if (!pending) return
        val (code, message) = apply(requested)
        when (code) {
            0 -> { active = requested; pending = false; notice = "" }
            3, 8 -> notice = "Waiting for the device to apply network settings…"
            else -> { pending = false; notice = "Couldn’t apply network settings: $message. Saved for next start." }
        }
    }

    @Synchronized fun snapshot() = Snapshot(active, notice)
}
