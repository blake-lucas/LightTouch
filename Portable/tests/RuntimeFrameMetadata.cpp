// Compile the actual public entry points with access to their private opaque
// runtime fixture. No engine library is loaded and no worker is started. The
// static archive supplies other domain operations, not a substitute metadata API.
#include "../src/Runtime.cpp"
#include <cassert>

#ifdef LIGHTTOUCH_TEST_WRAP_THROW
static uint64_t throws = 0;
extern "C" [[noreturn]] void __real___cxa_throw(void *, void *, void (*)(void *));
extern "C" [[noreturn]] void __wrap___cxa_throw(void *exception, void *type, void (*destroy)(void *)) {
    ++throws;
    __real___cxa_throw(exception, type, destroy);
}
#endif

static lt_frame_info metadata(uint64_t sequence) {
    return {sizeof(lt_frame_info), LT_ABI_VERSION, 2, 1, 8, 1, sequence, sequence * 11, 8};
}

int main() {
    // Live policy cannot open an offline boot, accept invalid settings, or
    // schedule engine work before start/after shutdown admission.
    {
        auto offline = std::make_unique<lt_runtime>(nullptr);
        lt_error error{sizeof(error), LT_ABI_VERSION, 0, {}};
        assert(lt_runtime_network_policy(nullptr, 1, 0, &error) == LT_AGAIN);
        assert(lt_runtime_network_policy(offline.get(), 3, 0, &error) == LT_INVALID);
        assert(lt_runtime_network_policy(offline.get(), 1, 2, &error) == LT_INVALID);
        assert(lt_runtime_network_policy(offline.get(), 1, 0, &error) == LT_AGAIN);
        offline->started = true;
        assert(lt_runtime_network_policy(offline.get(), 1, 0, &error) == LT_UNSUPPORTED);
        offline->stopping = true;
        assert(lt_runtime_network_policy(offline.get(), 1, 0, &error) == LT_AGAIN);
    }
    // Exercise the actual Stop/cancel entry points without any engine: after
    // Stop owns input, the watchdog's immediate cancel and later surface-loss
    // cancels must not enqueue an engine-wide release that clips its wake pulse.
    {
        auto stopping = std::make_unique<lt_runtime>(nullptr);
        stopping->started = true;
        lt_error error{sizeof(error), LT_ABI_VERSION, 0, {}};
        assert(lt_runtime_request_shutdown(stopping.get(), &error) == LT_OK);
        assert(stopping->stopping && stopping->shutdown_requested);
        for (unsigned i = 0; i < 100; ++i)
            assert(lt_runtime_cancel_input(stopping.get(), &error) == LT_OK);
        assert(lt_runtime_button(stopping.get(), 0, 1, &error) == LT_AGAIN);
        assert(lt_runtime_pause(stopping.get(), 1, &error) == LT_AGAIN);
        assert(lt_runtime_request_shutdown(stopping.get(), &error) == LT_OK);
        stopping->done = true;
        assert(lt_runtime_destroy(stopping.release(), &error) == LT_OK);
    }
#ifdef LIGHTTOUCH_TEST_WRAP_THROW
    throws = 0; // The following assertions measure only normal metadata polling.
#endif
    auto runtime = std::make_unique<lt_runtime>(nullptr);
    runtime->mapping_bytes = sizeof(lt_shared_frame) + 8;
    runtime->shared = static_cast<lt_shared_frame *>(mmap(nullptr, runtime->mapping_bytes,
        PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
    assert(runtime->shared != MAP_FAILED);
    *runtime->shared = {sizeof(lt_shared_frame), LT_ABI_VERSION, 0, {}, LT_FRAME_CAPACITY};
    runtime->frame_count = 17;
    runtime->frame_sequence = 13;
    runtime->frame_timestamp = 143;
    runtime->frame_published = 19;
    runtime->pcm_captured = 23;
    runtime->pcm_pulled = 29;
    runtime->pcm.dropped = 31;
    runtime->input_id = 37;

    lt_error error{sizeof(error), LT_ABI_VERSION, 0, {}};
    lt_frame_info out = metadata(999);
    const auto original = out;
    lt_runtime_io_stats before{};
    before.size = sizeof(before);
    before.version = LT_ABI_VERSION;
    assert(lt_runtime_io_statistics(runtime.get(), &before, &error) == LT_OK);
    auto unchanged = [&] {
        lt_runtime_io_stats after{};
        after.size = sizeof(after);
        after.version = LT_ABI_VERSION;
        assert(lt_runtime_io_statistics(runtime.get(), &after, &error) == LT_OK);
        assert(memcmp(&after, &before, sizeof(after)) == 0);
    };
    auto again = [&](uint64_t since) {
        assert(lt_runtime_frame_metadata(runtime.get(), since, &out, &error) == LT_AGAIN);
        assert(error.code == LT_AGAIN && std::string(error.message) == "no newer coherent published frame");
        assert(memcmp(&out, &original, sizeof(out)) == 0);
    };
    for (unsigned i = 0; i < 1000; ++i)
        again(0); // No publication: repeated expected polling leaves output/counters untouched.
#ifdef LIGHTTOUCH_TEST_WRAP_THROW
    assert(throws == 0); // Fails with the old expected-AGAIN exception path.
#endif
    unchanged();

    const std::array<uint8_t, 8> pixels{1, 2, 3, 4, 5, 6, 7, 8};
    const auto published = metadata(7);
    assert(lt::publish_frame(runtime->shared, published, pixels.data(), pixels.size()));
    assert(lt_runtime_frame_metadata(runtime.get(), 6, &out, &error) == LT_OK);
    assert(error.code == LT_OK && error.message[0] == '\0');
    assert(memcmp(&out, &published, sizeof(out)) == 0);
    unchanged(); // Metadata queries do not advance producer or consumption counters.
    out = original;
    for (unsigned i = 0; i < 1000; ++i)
        again(7);
    again(8);
    again(UINT64_MAX); // Since is by value; an older frame cannot be returned as new.
    assert(lt_runtime_frame_metadata(runtime.get(), 7, &out, nullptr) == LT_AGAIN);
    assert(memcmp(&out, &original, sizeof(out)) == 0);
#ifdef LIGHTTOUCH_TEST_WRAP_THROW
    assert(throws == 0);
#endif

    // An interrupted publication is expected retry, not malformed metadata.
    __atomic_add_fetch(&runtime->shared->generation, 1, __ATOMIC_SEQ_CST);
    auto invalid = published;
    invalid.bytes = LT_FRAME_CAPACITY + 1;
    lt::store_frame_metadata(runtime->shared->frame, invalid);
    again(0);
#ifdef LIGHTTOUCH_TEST_WRAP_THROW
    assert(throws == 0); // An unstable publication is also normal polling.
#endif
    __atomic_add_fetch(&runtime->shared->generation, 1, __ATOMIC_SEQ_CST);
    assert(lt_runtime_frame_metadata(runtime.get(), 0, &out, &error) == LT_ENGINE);
    assert(error.code == LT_ENGINE && std::string(error.message) == "invalid published frame metadata");
    assert(memcmp(&out, &original, sizeof(out)) == 0);

    // Runtime validation still precedes output ABI validation; output ABI
    // validation still precedes inspecting the malformed published metadata.
    assert(lt_runtime_frame_metadata(nullptr, 0, nullptr, &error) == LT_INVALID);
    assert(std::string(error.message) == "invalid frame metadata runtime");
    assert(lt_runtime_frame_metadata(runtime.get(), 0, nullptr, &error) == LT_INVALID);
    assert(std::string(error.message) == "invalid ABI size/version");
    out.size = 0;
    const auto invalid_output = out;
    assert(lt_runtime_frame_metadata(runtime.get(), 0, &out, &error) == LT_INVALID);
    assert(std::string(error.message) == "invalid ABI size/version");
#ifdef LIGHTTOUCH_TEST_WRAP_THROW
    assert(throws > 0); // Real validation failures prove the throw wrapper is active.
#endif
    assert(memcmp(&out, &invalid_output, sizeof(out)) == 0);
    out = original;
    out.version = LT_ABI_VERSION + 1;
    assert(lt_runtime_frame_metadata(runtime.get(), 0, &out, &error) == LT_INVALID);
    assert(std::string(error.message) == "invalid ABI size/version");
    const auto next = metadata(8);
    assert(lt::publish_frame(runtime->shared, next, pixels.data(), pixels.size()));
    out = original;
    assert(lt_runtime_frame_metadata(runtime.get(), 7, &out, &error) == LT_OK);
    assert(memcmp(&out, &next, sizeof(out)) == 0 && error.code == LT_OK && !error.message[0]);
    unchanged();
    assert(lt_runtime_destroy(runtime.release(), &error) == LT_OK);
}
