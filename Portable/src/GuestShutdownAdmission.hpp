#pragma once
#include <cstdint>
#include <limits>

namespace lt {
// Agent leases use host time and can expire while guest execution is paused.
// Poll from the publisher after resume/cancellation; never wait inside QEMU.
class GuestShutdownAdmission {
public:
    static constexpr uint64_t refresh_budget_ns = 5'000'000'000ull;
    // A Stop before this boot's agent first answers (iOS still starting) on a guest known to have one waits for it
    // (LT_SHUTDOWN_AGENT_WAIT_MS): Power pressed on a booting guest is lost, and the session ends dirty.
    static constexpr uint64_t boot_budget_ns = 20'000'000'000ull;
    static constexpr uint64_t wake_pulse_ns = 150'000'000ull;
    void observe(uint32_t agent) { seen_ = seen_ || agent == 1 || agent == 2; }
    void expect_agent() { expected_ = true; }
    // Home wakes a sleeping guest without toggling an awake display off. Only
    // do this for an agent-equipped guest, after VM resume and input cancel.
    // Return true exactly once when the caller must press Home.
    bool request(uint64_t now, bool sleeping = false) {
        if (pending_ || sent_) return false;
        pending_ = true;
        deadline_ = add(now, seen_ ? refresh_budget_ns : boot_budget_ns);
        wake_down_ = sleeping && seen_;
        wake_release_at_ = add(now, wake_pulse_ns);
        return wake_down_;
    }
    bool pending() const { return pending_; }
    bool wake_down() const { return wake_down_; }
    bool release_wake(uint64_t now) {
        if (!wake_down_ || now < wake_release_at_) return false;
        wake_down_ = false;
        return true;
    }
    bool admit(uint32_t agent, uint64_t now) {
        observe(agent);
        if (!pending_ || sent_ || wake_down_) return false;
        if (agent != 1 && (seen_ || expected_) && now < deadline_) return false;
        pending_ = false;
        sent_ = true;
        return true;
    }
private:
    static uint64_t add(uint64_t now, uint64_t delta) {
        return now > std::numeric_limits<uint64_t>::max() - delta
            ? std::numeric_limits<uint64_t>::max() : now + delta;
    }
    bool seen_ = false, expected_ = false, pending_ = false, sent_ = false, wake_down_ = false;
    uint64_t deadline_ = 0, wake_release_at_ = 0;
};
}
