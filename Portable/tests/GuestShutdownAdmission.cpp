#include "../src/GuestShutdownAdmission.hpp"
#include <cassert>
#include <limits>

int main() {
    using lt::GuestShutdownAdmission;
    constexpr uint64_t second = 1'000'000'000ull;
    // A paused guest's stale lease must not choose the broken fallback 10ms
    // after resume. Once the real daemon polls, submit exactly once.
    GuestShutdownAdmission paused;
    paused.observe(1); paused.observe(2); paused.request(100 * second);
    assert(!paused.admit(2, 100 * second + 10'000'000));
    assert(paused.pending());
    assert(paused.admit(1, 101 * second));
    assert(!paused.admit(1, 102 * second));
    paused.request(103 * second); assert(!paused.admit(1, 103 * second));

    // Cancelling an outstanding guest RPC can clear the agent claim. An alive
    // pre-cancel snapshot cannot authorize immediate fallback after that reset.
    GuestShutdownAdmission cancelled;
    cancelled.observe(1); cancelled.request(0);
    assert(!cancelled.admit(0, second));
    assert(cancelled.admit(1, 2 * second));

    // No agent has ever been present on older boards: keep immediate fallback.
    GuestShutdownAdmission absent;
    absent.observe(0); absent.request(7 * second);
    assert(absent.admit(0, 7 * second));

    // A missing daemon cannot postpone the original 35s disposal policy.
    // Repeated requests do not move the fixed five-second refresh limit.
    GuestShutdownAdmission lost;
    lost.observe(2); lost.request(10 * second); lost.request(14 * second);
    assert(!lost.admit(0, 15 * second - 1));
    assert(lost.admit(0, 15 * second));
    assert(!lost.admit(0, 16 * second));

    GuestShutdownAdmission overflow;
    const auto maximum = std::numeric_limits<uint64_t>::max();
    overflow.observe(2); overflow.request(maximum - 1);
    assert(!overflow.admit(2, maximum - 1));
    assert(overflow.admit(2, maximum));

    // An asleep guest cannot refresh just because the VM is running. Wake it
    // once, balance Home before shutdown admission, and retain the fixed cap.
    GuestShutdownAdmission sleeping;
    sleeping.observe(1); sleeping.observe(2);
    assert(sleeping.request(10 * second, true));
    assert(sleeping.wake_down());
    assert(!sleeping.request(11 * second, true));
    assert(!sleeping.admit(1, 10 * second));
    assert(!sleeping.release_wake(10 * second + GuestShutdownAdmission::wake_pulse_ns - 1));
    assert(sleeping.release_wake(10 * second + GuestShutdownAdmission::wake_pulse_ns));
    assert(!sleeping.release_wake(11 * second));
    assert(!sleeping.wake_down());
    assert(sleeping.admit(1, 11 * second));
    assert(!sleeping.request(12 * second, true));

    GuestShutdownAdmission sleepy_lost;
    sleepy_lost.observe(2);
    assert(sleepy_lost.request(10 * second, true));
    assert(sleepy_lost.release_wake(11 * second));
    assert(!sleepy_lost.admit(2, 15 * second - 1));
    assert(sleepy_lost.admit(2, 15 * second));

    GuestShutdownAdmission old_board;
    assert(!old_board.request(0, true)); // Never observed an agent: do not wake.
    assert(old_board.admit(0, 0));
    GuestShutdownAdmission awake;
    awake.observe(1);
    assert(!awake.request(0, false));
    assert(awake.admit(1, 0));

    // Stop during boot on a guest whose agent answered on earlier boots: wait for this boot's agent (no wake: the
    // display isn't asleep, iOS is starting), then admit it; give up at the boot budget.
    GuestShutdownAdmission booting;
    booting.expect_agent(); booting.observe(0);
    assert(!booting.request(3 * second, true));
    assert(!booting.admit(0, 3 * second));
    assert(!booting.admit(0, 15 * second));
    assert(booting.admit(1, 16 * second));
    GuestShutdownAdmission never_up;
    never_up.expect_agent();
    never_up.request(3 * second);
    assert(!never_up.admit(0, 3 * second + GuestShutdownAdmission::boot_budget_ns - 1));
    assert(never_up.admit(0, 3 * second + GuestShutdownAdmission::boot_budget_ns));
    // Once seen, a lost lease keeps the short refresh budget.
    GuestShutdownAdmission expected_stale;
    expected_stale.expect_agent(); expected_stale.observe(2); expected_stale.request(0);
    assert(expected_stale.admit(2, GuestShutdownAdmission::refresh_budget_ns));

    GuestShutdownAdmission sleepy_overflow;
    sleepy_overflow.observe(2);
    assert(sleepy_overflow.request(maximum - 1, true));
    assert(!sleepy_overflow.release_wake(maximum - 1));
    assert(!sleepy_overflow.admit(1, maximum));
    assert(sleepy_overflow.release_wake(maximum));
    assert(sleepy_overflow.admit(2, maximum));
}
