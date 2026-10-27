#include "Internal.hpp"
#include "MotionClock.hpp"
#include "Lifecycle.hpp"
#include "PcmEpoch.hpp"
#include "VmPauseControl.hpp"
#include "GuestShutdownAdmission.hpp"
#include "GuestPackageJudge.hpp"
#include "FrameStorage.hpp"
#include "Base64.hpp"
#include "GuestNetwork.hpp"
#include "qemu-ios-ui.h"
#include "qemu-macos-extras.h"
#include <atomic>
#include <array>
#include <chrono>
#include <condition_variable>
#include <cmath>
#include <deque>
#include "Os.hpp"
#include <memory>
#include <mutex>
#include <thread>
#ifndef _WIN32
#include <dlfcn.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#endif
#include <time.h>
#include <limits>
#include <algorithm>
#include <climits>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>
#ifdef __linux__
#include <sched.h>
#endif
namespace lt {
/*
 * Keep the engine on the high-capacity cores of a heterogeneous (big.LITTLE)
 * CPU. QEMU creates its vCPU, RCU and I/O threads from the calling thread, so
 * they inherit this mask. The guest's load is bursty (a timer interrupt every
 * millisecond, then work), which the Android scheduler reads as light load and
 * places on little cores: on a Poco F3 that made a cold boot 1.4-1.7x slower
 * than pinning to cores 4-7, and 5x slower when it stayed on cores 0-3.
 * Cores at least half as capable as the biggest are kept, so the vCPU and the
 * main loop never share one prime core. No-op on symmetric CPUs and when the
 * kernel exposes no capacities. LT_ENGINE_CPUS=off disables it.
 */
static void prefer_fast_cores() {
#ifdef __linux__
    const char *env = getenv("LT_ENGINE_CPUS");
    if (env && !strcmp(env, "off"))
        return;
    std::vector<long> capacity;
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        char path[96];
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%d", cpu);
        if (!lt::os::exists(path))
            break;
        snprintf(path, sizeof path, "/sys/devices/system/cpu/cpu%d/cpu_capacity", cpu);
        long value = 0;
        if (FILE *f = fopen(path, "re")) {
            if (fscanf(f, "%ld", &value) != 1)
                value = 0;
            fclose(f);
        }
        capacity.push_back(value);  /* 0: unknown (e.g. offline), never selected */
    }
    long biggest = 0, smallest = LONG_MAX;
    for (long c : capacity) {
        biggest = std::max(biggest, c);
        if (c > 0)
            smallest = std::min(smallest, c);
    }
    if (capacity.empty() || biggest <= 0 || smallest * 2 > biggest)
        return;
    /* Not intersected with sched_getaffinity(): that reports only the cores
     * active right now, and Qualcomm's core_ctl isolates idle big cores, which
     * would pin us away from exactly the cores we want. The kernel applies the
     * cpuset itself, and core_ctl unisolates cores once our threads load them. */
    cpu_set_t fast;
    CPU_ZERO(&fast);
    for (size_t cpu = 0; cpu < capacity.size(); cpu++)
        if (capacity[cpu] * 2 >= biggest)
            CPU_SET(cpu, &fast);
    if (CPU_COUNT(&fast) > 0)
        sched_setaffinity(0, sizeof fast, &fast);
#endif
}
static uint64_t now_ns() {
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return uint64_t(ts.tv_sec) * 1000000000ull + uint64_t(ts.tv_nsec);
}
struct Engine {
    lt::os::Library library = nullptr;
    decltype(&qemu_ios_main) main = nullptr;
    decltype(&qemu_ios_portable_board_at) board_at = nullptr;
    decltype(&qemu_ios_portable_board) board = nullptr;
    decltype(&qemu_ios_portable_frame_copy) frame = nullptr;
    decltype(&qemu_ios_portable_touch) touch = nullptr;
    decltype(&qemu_ios_portable_input_status) input_status = nullptr;
    decltype(&qemu_ios_portable_motion) motion = nullptr;
    decltype(&qemu_ios_portable_cancel_input) cancel = nullptr;
    decltype(&qemu_ios_ui_ready) ready = nullptr;
    decltype(&qemu_ios_ui_paused) paused = nullptr;
    decltype(&qemu_ios_ui_storage_failed) storage_failed = nullptr;
    decltype(&qemu_ios_ui_guest_shutdown_confirmed) shutdown_confirmed = nullptr;
    decltype(&qemu_ios_ui_display_sleeping) display_sleeping = nullptr;
    decltype(&qemu_ios_ui_backlight_level) backlight_level = nullptr;
    decltype(&qemu_ios_ui_pause) pause = nullptr;
    decltype(&qemu_ios_ui_resume) resume = nullptr;
    decltype(&qemu_ios_ui_shutdown) shutdown = nullptr;
    decltype(&qemu_ios_ui_quit) quit = nullptr;
    decltype(&qemu_ios_ui_button) button = nullptr;
    decltype(&qemu_ios_ui_net_restrict) net_restrict = nullptr;
    decltype(&qemu_ios_ui_net_lan) net_lan = nullptr;
    decltype(&qemu_ios_agent_request) agent_request = nullptr;
    decltype(&qemu_ios_agent_result) agent_result = nullptr;
    decltype(&qemu_ios_agent_free_result) agent_free = nullptr;
    decltype(&qemu_ios_agent_cancel) agent_cancel = nullptr;
    decltype(&qemu_ios_agent_status) agent_status = nullptr;
    decltype(&qemu_ios_audio_capture_start) audio_start = nullptr;
    decltype(&qemu_ios_audio_capture_read) audio_read = nullptr;
    decltype(&qemu_ios_audio_capture_stop) audio_stop = nullptr;
    decltype(&qemu_ios_profile_start) profile_start = nullptr;
    decltype(&qemu_ios_profile_stop) profile_stop = nullptr;
    decltype(&qemu_ios_profile_status) profile_status = nullptr;
    // Optional: older engines lack it and are polled instead.
    decltype(&qemu_ios_ui_attach) attach = nullptr;
    // Optional: engines before the guest-package hypercalls lack it.
    decltype(&qemu_ios_guest_package_report) package_report = nullptr;
    // Optional: engines before the storage counters lack it.
    decltype(&qemu_ios_storage_stats) storage_stats = nullptr;
    template <class F> F symbol(const char *n) {
        auto p = lt::os::library_symbol(library, n);
        require(p, std::string("engine symbol missing: ") + n, LT_UNSUPPORTED);
        return reinterpret_cast<F>(p);
    }
    explicit Engine(const char *path) {
        require(path && *path, "engine library missing");
        // QEMU registers process-global type/module callbacks at library load. Keep
        // code resident even when a discovery-only handle is released.
#ifdef _WIN32
        // Its dependencies sit beside it. Pinned, the RTLD_NODELETE of Windows: QEMU starts its RCU thread at load,
        // and releasing a discovery-only handle would unmap the code under it.
        std::string load_error;
        library = lt::os::library_open(path, load_error);
        if (!library)
            throw Failure(LT_ENGINE, "engine load failed: " + load_error);
        HMODULE pinned = nullptr;
        if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                                reinterpret_cast<LPCWSTR>(library), &pinned)) {
            lt::os::library_close(library);
            library = nullptr;
            throw Failure(LT_ENGINE, "engine load failed: could not pin " + std::string(path));
        }
#else
        int flags = RTLD_NOW | RTLD_GLOBAL;
#ifdef RTLD_NODELETE
        flags |= RTLD_NODELETE;
#endif
        library = dlopen(path, flags);
        if (!library) {
            const char *e = dlerror();
            throw Failure(LT_ENGINE, e ? e : "engine load failed");
        }
#endif
        try {
            auto version = symbol<decltype(&qemu_ios_api_version)>("qemu_ios_api_version");
            require((version() >> 16) == 2, "incompatible qemu_ios API major", LT_UNSUPPORTED);
            auto portable = symbol<decltype(&qemu_ios_portable_version)>("qemu_ios_portable_version");
            require(portable() == 1, "incompatible portable engine ABI", LT_UNSUPPORTED);
#define LOAD(member, name) member = symbol<decltype(member)>(#name)
            LOAD(main, qemu_ios_main);
            LOAD(board_at, qemu_ios_portable_board_at);
            LOAD(board, qemu_ios_portable_board);
            LOAD(frame, qemu_ios_portable_frame_copy);
            LOAD(touch, qemu_ios_portable_touch);
            LOAD(input_status, qemu_ios_portable_input_status);
            LOAD(motion, qemu_ios_portable_motion);
            LOAD(cancel, qemu_ios_portable_cancel_input);
            LOAD(ready, qemu_ios_ui_ready);
            LOAD(paused, qemu_ios_ui_paused);
            LOAD(storage_failed, qemu_ios_ui_storage_failed);
            LOAD(shutdown_confirmed, qemu_ios_ui_guest_shutdown_confirmed);
            LOAD(display_sleeping, qemu_ios_ui_display_sleeping);
            LOAD(backlight_level, qemu_ios_ui_backlight_level);
            LOAD(pause, qemu_ios_ui_pause);
            LOAD(resume, qemu_ios_ui_resume);
            LOAD(shutdown, qemu_ios_ui_shutdown);
            LOAD(quit, qemu_ios_ui_quit);
            LOAD(button, qemu_ios_ui_button);
            LOAD(agent_request, qemu_ios_agent_request);
            LOAD(agent_result, qemu_ios_agent_result);
            LOAD(agent_free, qemu_ios_agent_free_result);
            LOAD(agent_cancel, qemu_ios_agent_cancel);
            LOAD(agent_status, qemu_ios_agent_status);
            LOAD(audio_start, qemu_ios_audio_capture_start);
            LOAD(audio_read, qemu_ios_audio_capture_read);
            LOAD(audio_stop, qemu_ios_audio_capture_stop);
#undef LOAD
            profile_start = reinterpret_cast<decltype(profile_start)>(lt::os::library_symbol(library, "qemu_ios_profile_start"));
            profile_stop = reinterpret_cast<decltype(profile_stop)>(lt::os::library_symbol(library, "qemu_ios_profile_stop"));
            profile_status = reinterpret_cast<decltype(profile_status)>(lt::os::library_symbol(library, "qemu_ios_profile_status"));
            attach = reinterpret_cast<decltype(attach)>(lt::os::library_symbol(library, "qemu_ios_ui_attach"));
            net_restrict = reinterpret_cast<decltype(net_restrict)>(lt::os::library_symbol(library, "qemu_ios_ui_net_restrict"));
            net_lan = reinterpret_cast<decltype(net_lan)>(lt::os::library_symbol(library, "qemu_ios_ui_net_lan"));
            package_report =
                reinterpret_cast<decltype(package_report)>(lt::os::library_symbol(library, "qemu_ios_guest_package_report"));
            storage_stats =
                reinterpret_cast<decltype(storage_stats)>(lt::os::library_symbol(library, "qemu_ios_storage_stats"));
        } catch (...) {
            lt::os::library_close(library);
            library = nullptr;
            throw;
        }
    }
    ~Engine() {
        if (library)
            lt::os::library_close(library);
    }
    QemuIosPortableBoard find_board(const std::string &id) {
        for (int index = 0; index < 128; index++) {
            QemuIosPortableBoard facts{sizeof(facts), 1, {}, {}, 0, 0, 0, 0, 0, 0};
            int result = board_at(index, &facts);
            if (result == QEMU_IOS_UNSUPPORTED)
                break;
            require(result == QEMU_IOS_OK, "engine board discovery failed", LT_ENGINE);
            if (id == facts.board || id == facts.machine)
                return facts;
        }
        throw Failure(LT_UNSUPPORTED, "engine does not expose board: " + id);
    }
};
static void result(int r) {
    if (r == QEMU_IOS_OK || r == QEMU_IOS_QUEUED)
        return;
    if (r == QEMU_IOS_UNSUPPORTED)
        throw Failure(LT_UNSUPPORTED, "guest board does not support this operation");
    if (r == QEMU_IOS_NOT_READY || r == QEMU_IOS_BUSY || r == QEMU_IOS_STALE)
        throw Failure(LT_AGAIN, "engine not ready, busy, or stale input");
    throw Failure(LT_INVALID, "engine rejected operation: " + std::to_string(r));
}
static std::atomic<bool> process_started{false};
struct PcmPacket {
    std::array<int16_t, 8192> samples{};
    uint32_t bytes = 0;
    uint64_t timestamp = 0;
};
} // namespace lt
struct lt_runtime {
    lt_device *device;
    std::unique_ptr<lt::Engine> engine;
    std::thread guest, publisher;
    std::atomic<bool> done{false}, started{false}, stopping{false}, confirmed{false}, storage_failed{false},
        publisher_running{false};
    std::atomic<int> exit_code{0};
    std::atomic<bool> shutdown_requested{false}, shutdown_reply_received{false}, worker_failed{false};
    std::atomic<uint32_t> shutdown_phase{0}, shutdown_route{0}, agent_state{0};
    std::atomic<int32_t> shutdown_reply_status{0};
    std::atomic<uint64_t> frame_count{0};
    std::atomic<uint64_t> frame_sequence{0}, frame_timestamp{0}, frame_published{0}, pcm_captured{0}, pcm_pulled{0};
    std::mutex mutex;
    // The engine signals each captured frame (on QEMU's main loop, holding the
    // BQL) so the publisher copies it at once instead of on its next 10 ms poll.
    std::mutex wake_mutex;
    std::condition_variable wake;
    uint64_t engine_frames = 0;
    // Called on the publisher thread after each publication; see the header.
    std::mutex listener_mutex;
    lt_frame_listener listener = nullptr;
    void *listener_opaque = nullptr;
    static void engine_frame(void *opaque) {
        auto *r = static_cast<lt_runtime *>(opaque);
        {
            std::lock_guard<std::mutex> lock(r->wake_mutex);
            r->engine_frames++;
        }
        r->wake.notify_one();
    }
    int frame_fd = -1;
    // Perf work: LT_FRAME_LOG=path appends "monotonic_ns sequence" for each published frame
    // (tools/perf/framestats.py turns it into fps percentiles, hitches and a chart).
    FILE *frame_log = nullptr;
    int debug_qmp_fd = -1;
    std::string debug_qmp_path, debug_qmp_endpoint;
    lt_shared_frame *shared = nullptr;
    size_t mapping_bytes = 0;
    std::vector<char *> argv;
    bool debug_touch_trace = false;
    std::vector<uint8_t> scratch;
    uint64_t serial = 0, input_id = 0;
    lt::PcmCaptureEpoch audio_capture;
    lt::MotionClock motion_clock;
    uint64_t guest_sequence = 0, guest_deadline = 0;
    std::string guest_wire_id;
    lt_guest_result guest_result{sizeof(lt_guest_result), 1, 0, 0, 0, 0, 0, {}};
    uint64_t guest_capabilities = 0;
    lt::PcmEpochQueue<lt::PcmPacket> pcm;
    lt::VmPauseControl vm_pause;
    bool audio_enabled = false;
    bool network_available = false;
    std::string fault;
    explicit lt_runtime(lt_device *d) : device(d) {}
    ~lt_runtime() {
        if (guest.joinable())
            guest.join();
        if (publisher.joinable())
            publisher.join();
        if (engine && engine->attach)
            engine->attach(nullptr, nullptr);
#ifdef _WIN32
        std::free(shared);
#else
        if (shared)
            munmap(shared, mapping_bytes);
#endif
        if (frame_fd >= 0)
            lt::os::close(frame_fd);
        if (frame_log)
            fclose(frame_log);
#ifndef _WIN32
        if (debug_qmp_fd >= 0) {
            lt::os::Stat st {};
            if (lt::os::lstat(debug_qmp_path.c_str(), &st) == 0 && S_ISSOCK(st.st_mode) && st.st_uid == lt::os::euid())
                lt::os::unlink(debug_qmp_path.c_str());
            lt::os::close(debug_qmp_fd);
        }
#endif
    }
    // Without verdicts the guest's loader reverts any package it was not told is good after two
    // boots and refuses it from then on (GuestPackageJudge).
    void judge_package(lt::GuestPackageJudge &judge, bool paused) {
        int64_t serial = -1;
        int32_t result = 0;
        bool reported = engine->package_report(&serial, &result);
        auto verdict = judge.observe(lt::now_ns(), !paused, agent_state.load(), reported, serial);
        if (verdict == lt::GuestPackageJudge::Verdict::none)
            return;
        bool good = verdict == lt::GuestPackageJudge::Verdict::good;
        fprintf(stderr, "lighttouch: guest package serial %lld (loader result %d) judged %s\n",
                (long long)judge.serial(), int(result), good ? "good" : "bad");
        try {
            lt::record_guest_verdict(device, good, judge.serial());
        } catch (const std::exception &e) {
            fprintf(stderr, "lighttouch: could not record the guest package verdict: %s\n", e.what());
        }
    }
    void publish() {
        bool quit_sent = false;
        static_assert(lt::GuestShutdownAdmission::boot_budget_ns == uint64_t(LT_SHUTDOWN_AGENT_WAIT_MS) * 1000000);
        lt::GuestShutdownAdmission shutdown_admission;
        // A device whose agent answered on an earlier boot gets one on this boot too: a Stop before it answers waits
        // for it (GuestShutdownAdmission::boot_budget_ns) rather than pressing Power on a booting guest.
        try {
            if (lt::guest_agent_expected(device))
                shutdown_admission.expect_agent();
        } catch (const std::exception &e) {
            fprintf(stderr, "lighttouch: could not read the guest package verdicts: %s\n", e.what());
        }
        lt::GuestPackageJudge package_judge;
        while (!done) {
            try {
                if (engine->ready()) {
                    const bool actual_paused = engine->paused();
                    lt::VmPauseControl::Action pause_action = lt::VmPauseControl::Action::none;
                    bool shutdown_ready = false;
                    {
                        std::lock_guard<std::mutex> lock(mutex);
                        pause_action = vm_pause.step(actual_paused, true); // Inside engine->ready() boundary.
                        shutdown_ready = stopping && vm_pause.capture_allowed(actual_paused);
                    }
                    if (pause_action == lt::VmPauseControl::Action::pause)
                        engine->pause();
                    else if (pause_action == lt::VmPauseControl::Action::resume)
                        engine->resume();
                    agent_state = uint32_t(engine->agent_status());
                    shutdown_admission.observe(agent_state.load());
                    if (engine->package_report && !stopping)
                        judge_package(package_judge, actual_paused);
                    if (shutdown_ready && shutdown_requested.exchange(false)) {
                        shutdown_phase = 2;
                        engine->cancel();
                        std::string cancelled;
                        {
                            std::lock_guard<std::mutex> lock(mutex);
                            if (guest_result.state == 1) {
                                cancelled = guest_wire_id;
                                guest_result.state = 4;
                            }
                        }
                        if (!cancelled.empty())
                            engine->agent_cancel(cancelled.c_str());
                        if (engine->paused())
                            engine->resume();
                        if (shutdown_admission.request(lt::now_ns(), engine->display_sleeping()))
                            engine->button(QEMU_IOS_BUTTON_HOME, true); // Wake without a power-button toggle.
                    }
                    // Balance our wake even if the pause-admission boundary
                    // changes. Route selection alone does not own release.
                    if (shutdown_admission.release_wake(lt::now_ns()))
                        engine->button(QEMU_IOS_BUTTON_HOME, false);
                    if (shutdown_ready && shutdown_admission.pending()) {
                        // Cancellation can revoke a previously alive agent's
                        // claim. Read again instead of using the pre-cancel poll.
                        agent_state = uint32_t(engine->agent_status());
                        if (shutdown_admission.admit(agent_state.load(), lt::now_ns())) {
                            shutdown_phase = 3;
                            int route = engine->shutdown();
                            lt::require(route > 0, "engine did not admit guest shutdown", LT_ENGINE);
                            shutdown_route = uint32_t(route);
                            shutdown_phase = 4;
                        } else {
                            shutdown_phase = 6; // Refresh within the original shutdown deadline.
                        }
                    }
                    if (stopping && shutdown_route == 1 && !shutdown_reply_received) {
                        char *raw = engine->agent_result();
                        if (raw) {
                            std::unique_ptr<char, decltype(engine->agent_free)> reply(raw, engine->agent_free);
                            std::string text(raw, strnlen(raw, 8192));
                            const std::string prefix = "qemu-ios-shutdown ";
                            if (text.rfind(prefix, 0) == 0) {
                                size_t end = text.find('\n');
                                lt::require(end != std::string::npos, "invalid guest shutdown reply");
                                size_t parsed = 0;
                                long status = std::stol(text.substr(prefix.size(), end - prefix.size()), &parsed);
                                lt::require(parsed == end - prefix.size() && status >= INT32_MIN && status <= INT32_MAX,
                                            "invalid shutdown reply status");
                                shutdown_reply_status = int32_t(status);
                                shutdown_reply_received = true;
                            }
                        }
                    }
                    if (engine->storage_failed())
                        storage_failed = true;
                    if (engine->shutdown_confirmed()) {
                        confirmed = true;
                        shutdown_phase = 5;
                        if (!quit_sent) {
                            engine->cancel();
                            engine->quit();
                            quit_sent = true;
                        }
                    }
                    lt_frame_info f{sizeof(f), 1, 0, 0, 0, 0, 0, 0, 0};
                    const auto acquired = lt::acquire_frame(
                        scratch, serial, f, [&](void *pixels, size_t capacity, uint64_t since, lt_frame_info &out) {
                            QemuIosPortableFrame meta{sizeof(meta), 1, 0, 0, 0, 0, 0, 0, 0};
                            const int result = engine->frame(pixels, capacity, since, &meta);
                            if (result == QEMU_IOS_NO_FRAME)
                                return lt::FrameCopy::no_frame;
                            if (result == QEMU_IOS_NOT_READY)
                                return lt::FrameCopy::not_ready;
                            if (result != QEMU_IOS_BUFFER_SMALL)
                                lt::result(result);
                            out = {sizeof(out), 1, meta.width, meta.height, meta.stride, meta.format,
                                   meta.serial, meta.timestamp_ns, meta.required_bytes};
                            return result == QEMU_IOS_OK ? lt::FrameCopy::copied : lt::FrameCopy::small;
                        });
                    lt::require(acquired != lt::FrameAcquire::invalid, "engine returned invalid frame", LT_ENGINE);
                    if (acquired == lt::FrameAcquire::copied) {
                        lt::require(lt::publish_frame(shared, f, scratch.data(), scratch.size()),
                                    "invalid shared frame publication", LT_ENGINE);
                        serial = f.sequence;
                        frame_sequence = f.sequence;
                        frame_timestamp = f.timestamp_ns;
                        frame_published = lt::now_ns();
                        frame_count++;
                        if (frame_log) {
                            fprintf(frame_log, "%llu %llu\n", (unsigned long long)frame_published.load(),
                                    (unsigned long long)f.sequence);
                            fflush(frame_log);
                        }
                        std::lock_guard<std::mutex> lock(listener_mutex);
                        if (listener)
                            listener(listener_opaque, f.sequence);
                    }
                    if (audio_enabled) {
                        uint64_t read_epoch;
                        bool suspended;
                        {
                            std::lock_guard<std::mutex> lock(mutex);
                            read_epoch = pcm.epoch;
                            suspended = pcm.suspended || stopping || !vm_pause.capture_allowed(actual_paused);
                        }
                        // Stop/start belongs to this publisher alone. A new
                        // capture generation synchronously clears the engine
                        // ring; stopping alone leaves its queued packets intact.
                        const bool vm_paused = actual_paused;
                        audio_capture.synchronize(read_epoch, suspended, vm_paused,
                                                  [&](uint64_t generation) { engine->audio_stop(generation); },
                                                  [&] { return engine->audio_start(); });
                        if (!suspended && !vm_paused && audio_capture.generation) {
                            for (int i = 0; i < 8; i++) {
                                lt::PcmPacket p;
                                double seconds = 0;
                                int bytes = engine->audio_read(audio_capture.generation, p.samples.data(),
                                                               int(sizeof p.samples), &seconds);
                                {
                                    std::lock_guard<std::mutex> lock(mutex);
                                    if (read_epoch != pcm.epoch || pcm.suspended || stopping) {
                                        if (bytes > 0 && bytes <= int(sizeof p.samples) && bytes % 4 == 0) {
                                            pcm_captured += bytes / 4;
                                            pcm.dropped += bytes / 4;
                                        }
                                        break;
                                    }
                                }
                                if (bytes < 0) {
                                    throw lt::Failure(LT_ENGINE, "engine PCM capture overflow/failure");
                                }
                                if (!bytes)
                                    break;
                                lt::require(bytes <= int(sizeof p.samples) && bytes % 4 == 0,
                                            "invalid engine PCM packet", LT_ENGINE);
                                p.bytes = uint32_t(bytes);
                                pcm_captured += p.bytes / 4;
                                p.timestamp = uint64_t(std::max(0.0, seconds) * 1e9);
                                std::lock_guard<std::mutex> lock(mutex);
                                pcm.append(read_epoch, std::move(p));
                            }
                        }
                    }
                }
            } catch (const std::exception &e) {
                {
                    std::lock_guard<std::mutex> lock(mutex);
                    fault = e.what();
                }
                worker_failed = true;
                if (engine->ready() && !quit_sent) {
                    if (shutdown_admission.wake_down())
                        engine->button(QEMU_IOS_BUTTON_HOME, false);
                    engine->cancel();
                    engine->quit();
                    quit_sent = true;
                }
            }
            {
                std::unique_lock<std::mutex> lock(wake_mutex);
                const uint64_t seen = engine_frames;
                wake.wait_for(lock, std::chrono::milliseconds(10), [&] { return engine_frames != seen; });
            }
        }
        if (shutdown_admission.wake_down() && engine->ready())
            engine->button(QEMU_IOS_BUTTON_HOME, false);
        try {
            lt::clean_device(device, confirmed && !storage_failed && !worker_failed && exit_code == 0);
        } catch (const std::exception &e) {
            std::lock_guard<std::mutex> lock(mutex);
            fault = e.what();
            storage_failed = true;
        }
        device->active = false;
        publisher_running = false;
    }
};
extern "C" {
lt_status lt_engine_board(const char *library, const char *board, lt_board_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(out);
        lt::require(board, "board required");
        lt::Engine engine(library);
        auto f = engine.find_board(board);
        static_assert(sizeof(lt_board_info) == sizeof(QemuIosPortableBoard));
        memcpy(out, &f, sizeof f);
    });
}
lt_status lt_runtime_create(const lt_runtime_config *c, lt_runtime **out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(c);
        lt::abi(&c->boot);
        lt::require(out && c->device, "runtime requires leased device");
        *out = nullptr;
        lt::require(!c->device->active, "device already active", LT_BUSY);
        lt::require(!lt::process_started, "engine process already consumed: restart in fresh process", LT_BUSY);
        auto runtime = std::make_unique<lt_runtime>(c->device);
        // The proxy forward exists whenever a socket was given (Storage.cpp), Off included, so policy can open live.
        runtime->network_available = c->boot.network_mode != 0 || c->boot.network_socket != nullptr;
        runtime->engine = std::make_unique<lt::Engine>(c->engine_library);
        auto facts = runtime->engine->find_board(c->device->lock.at("board").string());
        runtime->guest_capabilities = facts.capabilities;
        lt_boot_config boot = c->boot;
        if (boot.machine)
            lt::require(std::string(boot.machine) == facts.machine, "machine does not match prepared base");
        else
            boot.machine = facts.machine;
        int argc = 0;
        const char *const *argv = nullptr;
        lt_error nested{sizeof(nested), 1, 0, {}};
        lt_status status = lt_device_boot_argv(c->device, &boot, &argc, &argv, &nested);
        lt::require(status == LT_OK, nested.message, status);
        for (int i = 0; i < argc; i++)
            runtime->argv.push_back(const_cast<char *>(argv[i]));
        runtime->argv.push_back(nullptr);
        if (const char *log = getenv("LT_LOG_ARGV"); log && *log) {
            // The engine's command line, one argument a line, for reproducing a boot outside the app.
            for (int i = 0; i < argc; i++)
                fprintf(stderr, "argv[%d] %s\n", i, argv[i]);
        }
        runtime->audio_enabled = boot.audio_enabled;
        if (const char *log = getenv("LT_FRAME_LOG"); log && *log)
            runtime->frame_log = fopen(log, "a");
#ifdef SYS_memfd_create
        runtime->frame_fd = int(syscall(SYS_memfd_create, "lighttouch-frame", MFD_CLOEXEC | MFD_ALLOW_SEALING));
#elif defined(__APPLE__)
        // No memfd: a POSIX shared memory object unlinked at once is as private (names are at most 31 bytes).
        {
            char name[32];
            snprintf(name, sizeof name, "/ltf-%d-%llx", lt::os::pid(), (unsigned long long)(lt::now_ns() & 0xffffffffffull));
            runtime->frame_fd = shm_open(name, O_RDWR | O_CREAT | O_EXCL, 0600);
            if (runtime->frame_fd >= 0) {
                shm_unlink(name);
                fcntl(runtime->frame_fd, F_SETFD, FD_CLOEXEC);
            }
        }
#elif !defined(_WIN32)
        runtime->frame_fd = -1;
        errno = ENOSYS;
#endif
        runtime->mapping_bytes = sizeof(lt_shared_frame) + LT_FRAME_CAPACITY;
#ifdef _WIN32
        // Nothing reopens the frame by descriptor here (lt_runtime_frame_fd is unsupported): private memory does.
        runtime->shared = static_cast<lt_shared_frame *>(std::calloc(1, runtime->mapping_bytes));
        lt::io(runtime->shared != nullptr, "allocate frame memory");
#else
        lt::io(runtime->frame_fd >= 0, "private frame memfd");
        lt::io(lt::os::ftruncate(runtime->frame_fd, int64_t(runtime->mapping_bytes)) == 0, "size frame memfd");
        runtime->shared = static_cast<lt_shared_frame *>(
            mmap(nullptr, runtime->mapping_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, runtime->frame_fd, 0));
        if (runtime->shared == MAP_FAILED) {
            runtime->shared = nullptr;
            throw lt::Failure(LT_IO, "map frame memfd");
        }
#endif
        runtime->shared->size = sizeof(lt_shared_frame);
        runtime->shared->version = 1;
        runtime->shared->capacity = LT_FRAME_CAPACITY;
#ifdef F_ADD_SEALS
        lt::io(fcntl(runtime->frame_fd, F_ADD_SEALS, F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL) == 0,
               "seal frame size");
#endif
        *out = runtime.release();
    });
}
lt_status lt_runtime_debug_touch_trace_enable(lt_runtime *r, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
#ifndef LIGHTTOUCH_ENABLE_DEBUG_QMP
        lt::require(false, "touch wire diagnosis is not compiled in", LT_UNSUPPORTED);
#else
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(!r->started && !r->done, "touch trace must be enabled before start", LT_BUSY);
        if (r->debug_touch_trace)
            return;
        r->argv.reserve(r->argv.size() + 2);
        r->argv.pop_back();
        r->argv.push_back(const_cast<char *>("-trace"));
        r->argv.push_back(const_cast<char *>("enable=ipod_touch_multitouch_log"));
        r->argv.push_back(nullptr);
        r->debug_touch_trace = true;
#endif
    });
}
lt_status lt_runtime_debug_qmp_enable(lt_runtime *r, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
#ifndef LIGHTTOUCH_ENABLE_DEBUG_QMP
        throw lt::Failure(LT_UNSUPPORTED, "private QMP instrumentation disabled in this build");
#else
        lt::require(!r->started && !lt::process_started, "debug QMP must be enabled before start", LT_BUSY);
        if (r->debug_qmp_fd >= 0)
            return;
        auto work = r->device->state / "work";
        lt::fs::create_directories(work);
        struct OwnedFd {
            int value;
            ~OwnedFd() { if (value >= 0) lt::os::close(value); }
        } fd{lt::os::open(work.c_str(), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)};
        lt::io(fd.value >= 0, "open private QMP work directory");
        lt::os::Stat st{};
        lt::io(lt::os::fstat(fd.value, &st) == 0, "inspect private QMP directory");
        lt::require(st.st_uid == lt::os::euid(), "QMP directory owner mismatch");
        lt::io(lt::os::fchmod(fd.value, 0700) == 0, "protect private QMP directory");
        std::string name = "qmp-" + std::to_string(getpid()) + ".sock";
        std::string path = (work / name).string();
        if (lt::os::lstat(path.c_str(), &st) == 0) {
            lt::require(S_ISSOCK(st.st_mode) && st.st_uid == lt::os::euid(), "unexpected QMP path occupant");
            lt::io(lt::os::unlink(path.c_str()) == 0, "remove stopped QMP socket");
        }
        r->argv.reserve(r->argv.size() + 2);
        r->debug_qmp_path = path;
        r->debug_qmp_endpoint = "unix:/proc/self/fd/" + std::to_string(fd.value) + "/" + name + ",server=on,wait=off";
        r->argv.pop_back();
        r->argv.push_back(const_cast<char *>("-qmp"));
        r->argv.push_back(r->debug_qmp_endpoint.data());
        r->argv.push_back(nullptr);
        r->debug_qmp_fd = fd.value;
        fd.value = -1;
#endif
    });
}
lt_status lt_runtime_debug_qmp_path(lt_runtime *r, char *path, uint32_t capacity, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && path, "debug QMP path output required");
        lt::require(r->debug_qmp_fd >= 0, "debug QMP is not enabled", LT_UNSUPPORTED);
        lt::require(r->debug_qmp_path.size() < capacity, "debug QMP path buffer too small");
        memcpy(path, r->debug_qmp_path.c_str(), r->debug_qmp_path.size() + 1);
    });
}
lt_status lt_runtime_start(lt_runtime *r, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::require(!lt::process_started, "one engine start per process; use fresh worker", LT_BUSY);
        lt::atomic_write(r->device->state / ".dirty", "schema=1\nbase=" + r->device->base_digest + "\n");
        r->device->dirty = true;
#ifdef _WIN32
        lt::io(_putenv_s("IT_TVOUT_READY", "1") == 0, "set shared boot environment");
#else
        lt::io(setenv("IT_TVOUT_READY", "1", 1) == 0, "set shared boot environment");
#endif
        bool expected = false;
        lt::require(lt::process_started.compare_exchange_strong(expected, true),
                    "one engine start per process; use fresh worker", LT_BUSY);
        r->device->active = true;
        r->started = true;
        // Start supervisor first. A guest thread creation failure leaves a terminal dirty
        // state; no destructor can block waiting on a guest without a supervisor.
        try {
            if (r->engine->attach)
                r->engine->attach(&lt_runtime::engine_frame, r);
            r->publisher_running = true;
            r->publisher = std::thread([r] { r->publish(); });
            r->guest = std::thread([r] {
                lt::prefer_fast_cores();
                int result = r->engine->main(int(r->argv.size() - 1), r->argv.data());
                if (r->engine->storage_failed())
                    r->storage_failed = true;
                if (r->engine->shutdown_confirmed())
                    r->confirmed = true;
                r->exit_code = result;
                r->done = true;
            });
        } catch (const std::exception &x) {
            r->exit_code = -1;
            r->storage_failed = true;
            r->done = true;
            if (r->publisher.joinable())
                r->publisher.join();
            r->publisher_running = false;
            r->device->active = false;
            throw lt::Failure(LT_ENGINE, std::string("worker startup failed: ") + x.what());
        }
    });
}
lt_status lt_runtime_poll(lt_runtime *r, lt_runtime_status *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::abi(out);
        auto issue = lt::terminal_issue(r->done, r->publisher_running, r->confirmed, r->device->dirty, r->exit_code);
        bool failed = r->storage_failed || r->worker_failed || issue != lt::TerminalIssue::none;
        out->state = !r->started           ? LT_STOPPED
                     : failed              ? LT_FAILED
                     : r->done             ? (r->publisher_running ? LT_STOPPING : LT_STOPPED)
                     : r->stopping         ? LT_STOPPING
                     : !r->engine->ready() ? LT_STARTING
                     : r->engine->paused() ? LT_PAUSED
                                           : LT_RUNNING;
        out->guest_shutdown_confirmed = r->confirmed;
        out->storage_failed = r->storage_failed;
        out->dirty = r->device->dirty;
        out->engine_exit = r->exit_code;
        out->frames = r->frame_count;
        std::lock_guard<std::mutex> lock(r->mutex);
        if (!r->fault.empty())
            throw lt::Failure(LT_ENGINE, r->fault);
        if (issue == lt::TerminalIssue::engine_exit)
            throw lt::Failure(LT_ENGINE, "engine exited with code " + std::to_string(r->exit_code) +
                                             "; state remains dirty; recover in a fresh worker");
        if (issue == lt::TerminalIssue::unconfirmed_shutdown)
            throw lt::Failure(LT_ENGINE, "engine returned without confirmed guest shutdown; state remains dirty; "
                                         "recover in a fresh worker");
        if (issue == lt::TerminalIssue::dirty_state)
            throw lt::Failure(LT_ENGINE, "guest shutdown was confirmed but state could not be finalized cleanly; "
                                         "recover in a fresh worker");
    });
}
lt_status lt_runtime_profile(lt_runtime *r, const char *path, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && r->started, "guest runtime inactive", LT_AGAIN);
        lt::require(r->engine->profile_start && r->engine->profile_stop,
                    "engine profiling unavailable", LT_UNSUPPORTED);
        bool start = path && *path;
        lt::require(!start || (!r->done && !r->stopping), "guest runtime stopping", LT_AGAIN);
        int code = start ? r->engine->profile_start(path) : r->engine->profile_stop();
        lt::require(code == 0, "Profile capture: " + std::string(strerror(-code)), LT_ENGINE);
    });
}
lt_status lt_runtime_profile_status(lt_runtime *r, int32_t *status, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && status, "guest runtime inactive", LT_AGAIN);
        lt::require(r->engine->profile_status, "engine profiling unavailable", LT_UNSUPPORTED);
        *status = r->engine->profile_status();
    });
}
lt_status lt_runtime_network_policy(lt_runtime *r, uint32_t mode, uint32_t local, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(mode <= 2 && local <= 1, "Invalid network policy");
        lt::require(r && r->started && !r->done && !r->stopping, "runtime not active", LT_AGAIN);
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(!r->stopping, "runtime is stopping", LT_AGAIN);
        lt::require(r->network_available, "This session started without networking; stop and start the device", LT_UNSUPPORTED);
        lt::require(r->engine->net_restrict && r->engine->net_lan, "Engine cannot change network policy live", LT_UNSUPPORTED);
        lt::require(r->engine->ready(), "Waiting for network readiness", LT_AGAIN);
        // wifi0 is the private user netdev created by lt_device_boot_argv.
        // These hooks schedule bottom halves; never enter slirp on the caller.
        // Close NAT before changing LAN admission, and open it last.
        if (mode != 1) r->engine->net_restrict("wifi0", true);
        r->engine->net_lan("wifi0", mode == 1 && local);
        if (mode == 1) r->engine->net_restrict("wifi0", false);
    });
}
lt_status lt_runtime_pause(lt_runtime *r, uint32_t paused, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && r->started && !r->done && !r->stopping, "runtime not active", LT_AGAIN);
        {
            std::lock_guard<std::mutex> lock(r->mutex);
            lt::require(!r->stopping, "runtime is stopping", LT_AGAIN);
            r->engine->cancel();
            r->motion_clock.reset();
            r->pcm.transition(paused != 0);
            r->vm_pause.request(paused != 0);
        }
    });
}
lt_status lt_runtime_request_shutdown(lt_runtime *r, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && r->started && !r->done, "runtime not active", LT_AGAIN);
        std::lock_guard<std::mutex> lock(r->mutex);
        if (!r->stopping.exchange(true)) {
            r->pcm.transition(true);
            r->vm_pause.shutdown_target();
            r->shutdown_phase = 1;
            r->shutdown_requested = true;
        }
    });
}
lt_status lt_runtime_diagnose(lt_runtime *r, lt_runtime_diagnostics *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::abi(out);
        out->engine_ready = r->engine->ready();
        out->display_sleeping = r->engine->display_sleeping();
        out->backlight_level = r->engine->backlight_level();
        out->agent_status = r->agent_state;
        out->shutdown_phase = r->shutdown_phase;
        out->shutdown_route = r->shutdown_route;
        out->shutdown_reply_status = r->shutdown_reply_status;
        out->shutdown_reply_received = r->shutdown_reply_received;
    });
}
lt_status lt_runtime_io_statistics(lt_runtime *r, lt_runtime_io_stats *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::abi(out);
        std::lock_guard<std::mutex> lock(r->mutex);
        *out = {sizeof(*out),
                1,
                r->frame_count,
                r->frame_sequence,
                r->frame_timestamp,
                r->frame_published,
                r->pcm_captured,
                r->pcm_pulled,
                r->pcm.dropped,
                r->input_id,
                uint32_t(r->pcm.packets.size()),
                0};
    });
}
lt_status lt_runtime_storage_statistics(lt_runtime *r, lt_runtime_storage_stats *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::abi(out);
        lt::require(r->engine->storage_stats, "engine has no storage counters", LT_UNSUPPORTED);
        QemuIosStorageStats s{};
        r->engine->storage_stats(&s);
        *out = {sizeof(*out), 1, s.read_ops, s.read_bytes, s.read_ns, s.write_ops, s.write_bytes, s.write_ns,
                s.erase_ops, s.erase_ns};
    });
}
lt_status lt_runtime_destroy(lt_runtime *r, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::require(!r->started || r->done, "wait for stop or terminate worker process", LT_BUSY);
        delete r;
    });
}
lt_status lt_runtime_copy_frame(lt_runtime *r, void *p, uint64_t capacity, uint64_t *seq, lt_frame_info *out,
                                lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && p && seq, "invalid frame output");
        lt::abi(out);
        lt::require(capacity <= SIZE_MAX, "frame capacity overflow");
        QemuIosPortableFrame f{sizeof(f), 1, 0, 0, 0, 0, 0, 0, 0};
        int result = r->engine->frame(p, size_t(capacity), *seq, &f);
        if (result == QEMU_IOS_NO_FRAME)
            throw lt::Failure(LT_AGAIN, "no newer frame");
        if (result == QEMU_IOS_BUFFER_SMALL)
            throw lt::Failure(LT_INVALID, "frame buffer too small; requires " + std::to_string(f.required_bytes));
        lt::result(result);
        *out = {sizeof(*out), 1, f.width, f.height, f.stride, f.format, f.serial, f.timestamp_ns, f.required_bytes};
        *seq = f.serial;
    });
}
lt_status lt_runtime_frame_metadata(lt_runtime *r, uint64_t since, lt_frame_info *out, lt_error *e) {
    bool ready = false;
    const auto status = lt::guard(e, [&] {
        lt::require(r, "invalid frame metadata runtime");
        lt::abi(out);
        const auto result = lt::read_frame_metadata(r->shared, since, *out);
        lt::require(result != lt::MetadataRead::invalid, "invalid published frame metadata", LT_ENGINE);
        ready = result == lt::MetadataRead::fresh;
    });
    if (status != LT_OK)
        return status;
    return ready ? status : lt::error(e, LT_AGAIN, "no newer coherent published frame");
}
lt_status lt_runtime_set_frame_listener(lt_runtime *r, lt_frame_listener listener, void *opaque, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "invalid frame listener runtime");
        std::lock_guard<std::mutex> lock(r->listener_mutex);
        r->listener = listener;
        r->listener_opaque = opaque;
    });
}
lt_status lt_runtime_frame_fd(lt_runtime *r, int *fd, uint64_t *bytes, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && fd && bytes, "invalid frame FD output");
#if defined(__APPLE__) || defined(_WIN32)
        // A shared memory object can't be reopened read-only by descriptor.
        throw lt::Failure(LT_UNSUPPORTED, "frame FD sharing is not available on this system");
#endif
        std::string path = "/proc/self/fd/" + std::to_string(r->frame_fd);
        int copy = lt::os::open(path.c_str(), O_RDONLY | O_CLOEXEC);
        lt::io(copy >= 0, "readonly frame FD");
        *fd = copy;
        *bytes = r->mapping_bytes;
    });
}
lt_status lt_runtime_write_screenshot(lt_runtime *r, const char *path, lt_frame_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && path, "screenshot runtime/path missing");
        lt::abi(out);
        QemuIosPortableFrame f{sizeof(f), 1, 0, 0, 0, 0, 0, 0, 0};
        int result = r->engine->frame(nullptr, 0, 0, &f);
        if (result == QEMU_IOS_NO_FRAME)
            throw lt::Failure(LT_AGAIN, "guest has not published a screenshot frame");
        lt::require(result == QEMU_IOS_BUFFER_SMALL, "frame metadata unavailable", LT_AGAIN);
        lt::require(f.required_bytes && f.required_bytes <= LT_FRAME_CAPACITY, "screenshot exceeds shared frame bound");
        std::vector<uint8_t> pixels(size_t(f.required_bytes));
        for (unsigned attempt = 0; attempt < 3; attempt++) {
            result = r->engine->frame(pixels.data(), pixels.size(), 0, &f);
            if (result == QEMU_IOS_BUFFER_SMALL) {
                lt::require(f.required_bytes <= LT_FRAME_CAPACITY, "screenshot grew beyond bound");
                pixels.resize(size_t(f.required_bytes));
                continue;
            }
            lt::result(result);
            *out = {sizeof(*out), 1, f.width, f.height, f.stride, f.format, f.serial, f.timestamp_ns, f.required_bytes};
            lt_error nested{sizeof(nested), 1, 0, {}};
            auto status = lt_frame_write_bmp(out, pixels.data(), pixels.size(), path, &nested);
            lt::require(status == LT_OK, nested.message, status);
            return;
        }
        throw lt::Failure(LT_AGAIN, "screenshot dimensions changed repeatedly; retry capture");
    });
}
lt_status lt_runtime_touch(lt_runtime *r, const lt_touch_batch *b, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && r->started && !r->done && !r->stopping, "input runtime inactive", LT_AGAIN);
        lt::abi(b);
        lt::require(b->count > 0 && b->count <= LT_MAX_CONTACTS, "invalid touch count");
        QemuIosPortableContact contacts[LT_MAX_CONTACTS]{};
        for (uint32_t i = 0; i < b->count; i++) {
            lt::abi(&b->contacts[i]);
            contacts[i] = {sizeof(contacts[i]), 1, b->contacts[i].id, b->contacts[i].phase, b->contacts[i].x,
                           b->contacts[i].y};
        }
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(!r->stopping, "input runtime inactive", LT_AGAIN);
        lt::result(r->engine->touch(++r->input_id, b->timestamp_ns, contacts, b->count));
    });
}
lt_status lt_runtime_input_status(lt_runtime *r, lt_input_status *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::abi(out);
        std::lock_guard<std::mutex> lock(r->mutex);
        out->request_id = r->input_id;
        out->engine_result = r->input_id ? r->engine->input_status(r->input_id) : QEMU_IOS_NOT_READY;
        out->state = !r->input_id                            ? 0
                     : out->engine_result == QEMU_IOS_QUEUED ? 1
                     : out->engine_result == QEMU_IOS_OK     ? 2
                                                             : 3;
    });
}
lt_status lt_runtime_cancel_input(lt_runtime *r, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        std::lock_guard<std::mutex> lock(r->mutex);
        // Stop owns its initial cancellation and subsequent wake pulse. A late
        // surface/watchdog cancellation must not clip that pulse or the fallback.
        if (!r->stopping)
            lt::result(r->engine->cancel());
        r->motion_clock.reset();
    });
}
lt_status lt_runtime_button(lt_runtime *r, uint32_t button, uint32_t down, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && r->started && !r->done && !r->stopping, "button runtime inactive", LT_AGAIN);
        lt::require(button <= 3 && down <= 1, "invalid hardware button");
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(!r->stopping, "button runtime inactive", LT_AGAIN);
        r->engine->button(int(button), down != 0);
    });
}
lt_status lt_runtime_motion(lt_runtime *r, const lt_motion *m, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && r->started && !r->done && !r->stopping && !r->engine->paused(), "motion inactive", LT_AGAIN);
        lt::abi(m);
        uint64_t now = lt::now_ns();
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(!r->stopping, "motion inactive", LT_AGAIN);
        r->motion_clock.validate(m->kinds, m->timestamp_ns, now);
        QemuIosPortableMotion sample{};
        static_assert(sizeof(sample) == sizeof(*m));
        memcpy(&sample, m, sizeof sample);
        lt::result(r->engine->motion(&sample));
        r->motion_clock.commit(m->kinds, m->timestamp_ns);
    });
}
lt_status lt_map_motion(uint32_t rotation, uint64_t timestamp, uint64_t source_now, const lt_motion *raw,
                        lt_motion *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(raw);
        lt::abi(out);
        lt::require(rotation == 0 || rotation == 90 || rotation == 180 || rotation == 270, "invalid motion rotation");
        lt::require(timestamp <= source_now && source_now - timestamp <= 250000000, "stale adapter motion", LT_AGAIN);
        lt_motion mapped = *raw;
        uint64_t now = lt::now_ns();
        lt::require(now >= source_now - timestamp, "invalid clock conversion");
        mapped.timestamp_ns = now - (source_now - timestamp);
        auto convert = [&](const double *in, double *dest) {
            double x = in[0], y = in[1], z = in[2];
            lt::require(std::isfinite(x) && std::isfinite(y) && std::isfinite(z), "nonfinite motion");
            switch (rotation) {
            case 0:
                dest[0] = x;
                dest[1] = y;
                break;
            case 90:
                dest[0] = -y;
                dest[1] = x;
                break;
            case 180:
                dest[0] = -x;
                dest[1] = -y;
                break;
            case 270:
                dest[0] = y;
                dest[1] = -x;
                break;
            }
            dest[2] = z;
        };
        convert(raw->acceleration, mapped.acceleration);
        convert(raw->angular_velocity, mapped.angular_velocity);
        *out = mapped;
    });
}
lt_status lt_map_contact(double px, double py, double left, double top, double width, double height, uint32_t rotation,
                         double *x, double *y) {
    if (!x || !y || !std::isfinite(px) || !std::isfinite(py) || !std::isfinite(left) || !std::isfinite(top) ||
        !std::isfinite(width) || !std::isfinite(height) || width <= 0 || height <= 0)
        return LT_INVALID;
    double nx = (px - left) / width, ny = (py - top) / height;
    if (nx < 0 || nx > 1 || ny < 0 || ny > 1)
        return LT_AGAIN;
    switch (rotation) {
    case 0:
        *x = nx;
        *y = ny;
        break;
    case 90:
        *x = ny;
        *y = 1 - nx;
        break;
    case 180:
        *x = 1 - nx;
        *y = 1 - ny;
        break;
    case 270:
        *x = 1 - ny;
        *y = nx;
        break;
    default:
        return LT_INVALID;
    }
    return LT_OK;
}
lt_status lt_runtime_pcm(lt_runtime *r, int16_t *out, uint32_t capacity, lt_pcm_info *info, lt_error *e) {
    bool ready = false;
    const auto status = lt::guard(e, [&] {
        lt::require(r && out, "invalid PCM output");
        lt::abi(info);
        lt::require(r->audio_enabled, "audio disabled", LT_UNSUPPORTED);
        lt::require(capacity >= 4096, "PCM capacity needs 4096 stereo frames");
        std::lock_guard<std::mutex> lock(r->mutex);
        ready = lt::pull_pcm(r->pcm, out, *info, r->pcm_pulled);
    });
    if (status != LT_OK)
        return status; // Validation/errors keep their original precedence.
    return ready ? status : lt::error(e, LT_AGAIN, "no PCM ready");
}
lt_status lt_runtime_guest_status(lt_runtime *r, uint32_t *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && out, "guest status output missing");
        *out = uint32_t(r->engine->agent_status());
    });
}
lt_status lt_runtime_guest_request(lt_runtime *r, const lt_guest_request *c, uint64_t *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r && out && r->started && !r->done && !r->stopping, "guest runtime inactive", LT_AGAIN);
        lt::abi(c);
        lt::require(r->engine->agent_status() == 1, "guest agent is absent or stale", LT_AGAIN);
        std::string argument = c->argument ? c->argument : "", op, body;
        lt::require(argument.size() < 4096, "guest argument exceeds limit");
        switch (c->operation) {
        case LT_GUEST_PING:
            op = "ping";
            lt::require(argument.empty(), "ping takes no argument");
            break;
        case LT_GUEST_FRONTMOST:
            op = "frontmost";
            lt::require(argument.empty(), "frontmost takes no argument");
            break;
        case LT_GUEST_SYNC:
            op = "sync";
            lt::require(argument.empty(), "sync takes no argument");
            break;
        case LT_GUEST_TYPE_TEXT:
            op = "type";
            body = argument;
            argument.clear();
            break;
        case LT_GUEST_NETWORK_CERTIFICATE:
            op = "put";
            body = lt::base64_decode(argument);
            lt::require(body.size() > 256 && body.size() < 4096, "Invalid proxy certificate length");
            argument = "/tmp/lighttouch-network-ca.der 644";
            break;
        case LT_GUEST_NETWORK_TRUST:
        case LT_GUEST_NETWORK_ROUTE: {
            lt::require(argument.empty(), "Network setup takes no argument");
            op = "spawn";
            std::vector<std::string> args = c->operation == LT_GUEST_NETWORK_TRUST ?
                std::vector<std::string>{"/tmp/lighttouch-ittrust", "add", "/tmp/lighttouch-network-ca.der"} :
                std::vector<std::string>{"/tmp/lighttouch-itproxy", "on"};
            for (const auto &arg : args) { body += arg; body += '\0'; }
            break;
        }
        case LT_GUEST_NETWORK_TRUST_CHUNK:
        case LT_GUEST_NETWORK_ROUTE_CHUNK:
            op = "putpart";
            argument = lt::network_tool_chunk(argument, c->operation == LT_GUEST_NETWORK_TRUST_CHUNK, body);
            break;
        case LT_GUEST_LAUNCH_BUILTIN:
            lt::require(argument.rfind("com.apple.", 0) == 0, "only builtin app IDs allowed");
            op = "launch";
            break;
        case LT_GUEST_LAUNCH_GYRO_PROBE:
            lt::require(r->guest_capabilities & QEMU_IOS_CAP_GYRO,
                        "guest gyro unavailable; authorized gyro workload remains blocked", LT_UNSUPPORTED);
            op = "launch";
            break;
        default:
            throw lt::Failure(LT_UNSUPPORTED, "unsupported typed guest operation");
        }
        if (op == "launch")
            lt::require(
                !argument.empty() && argument.size() < 256 &&
                    argument.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-") ==
                        std::string::npos,
                "invalid installed bundle ID");
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(!r->stopping, "guest runtime is stopping", LT_AGAIN);
        lt::require(r->guest_result.state != 1, "guest operation already pending", LT_BUSY);
        uint64_t id = ++r->guest_sequence;
        std::string wire_id = "lighttouch-" + std::to_string(id);
        std::string request =
            wire_id + " " + op + (argument.empty() ? "" : " " + argument) + "\n" + lt::base64_encode(body);
        lt::require(r->engine->agent_request(request.c_str()), "guest request not admitted", LT_AGAIN);
        r->guest_wire_id = wire_id;
        r->guest_result = {sizeof(lt_guest_result), 1, id, 1, 0, 0, 0, {}};
        r->guest_deadline = lt::now_ns() + uint64_t(c->timeout_ms ? std::min(c->timeout_ms, 65000u) : 65000u) * 1000000;
        *out = id;
    });
}
lt_status lt_runtime_guest_poll(lt_runtime *r, uint64_t id, lt_guest_result *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        lt::abi(out);
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(id && r->guest_result.request_id == id, "guest request no longer current", LT_INVALID);
        if (r->guest_result.state == 1) {
            for (unsigned i = 0; i < 8; i++) {
                char *raw = r->engine->agent_result();
                if (!raw)
                    break;
                std::unique_ptr<char, decltype(r->engine->agent_free)> owned(raw, r->engine->agent_free);
                size_t length = strnlen(raw, 8192);
                lt::require(length < 8192, "guest reply exceeds bound");
                std::string reply(raw, length);
                size_t space = reply.find(' '), newline = reply.find('\n');
                lt::require(space != std::string::npos && newline != std::string::npos && space < newline,
                            "invalid guest reply header");
                if (reply.substr(0, space) != r->guest_wire_id)
                    continue;
                std::string number = reply.substr(space + 1, newline - space - 1);
                size_t parsed = 0;
                long status = std::stol(number, &parsed);
                lt::require(parsed == number.size() && status >= INT32_MIN && status <= INT32_MAX,
                            "invalid guest reply status");
                auto body = lt::base64_decode(reply.substr(newline + 1));
                lt::require(body.size() < sizeof(r->guest_result.output), "guest output exceeds ABI limit");
                r->guest_result.state = status == 0 ? 2 : 3;
                r->guest_result.guest_status = int32_t(status);
                r->guest_result.bytes = uint32_t(body.size());
                memcpy(r->guest_result.output, body.c_str(), body.size() + 1);
                break;
            }
            if (r->guest_result.state == 1 && lt::now_ns() >= r->guest_deadline) {
                r->engine->agent_cancel(r->guest_wire_id.c_str());
                r->guest_result.state = 5;
            }
        }
        *out = r->guest_result;
    });
}
lt_status lt_runtime_guest_cancel(lt_runtime *r, uint64_t id, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(r, "null runtime");
        std::lock_guard<std::mutex> lock(r->mutex);
        lt::require(id && r->guest_result.request_id == id, "guest request no longer current");
        if (r->guest_result.state == 1) {
            r->engine->agent_cancel(r->guest_wire_id.c_str());
            r->guest_result.state = 4;
        }
    });
}
}
