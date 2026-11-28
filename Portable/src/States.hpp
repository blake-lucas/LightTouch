#pragma once
// Save states (States.cpp): the library side, shared by the runtime's save and load (Runtime.cpp).
#include "Internal.hpp"
#include "Zip.hpp"
#include <functional>

namespace lt::states {

// What the saving host is: a state's GL part is the host GL backend's (desktop GL, or Android's native GLES).
const char *host_kind();

// The record's states folder (records/<id>/states), beside its state/.
fs::path directory(const lt_device *);

// Rate-limited phase observer, as device transfer's: a callback per megabyte would wake a UI hundreds of times a
// second. A nonzero return cancels (LT_CANCELLED).
struct Observer {
    Observer(lt_import_phase_fn fn, void *opaque) : fn(fn), opaque(opaque) {}
    lt_import_phase_fn fn = nullptr;
    void *opaque = nullptr;
    std::string phase;
    uint64_t done = 0, total = 0;
    int64_t last_ms = 0;
    void begin(const char *name, uint64_t bytes);
    void add(uint64_t bytes);
    void report(bool force);
};

// A frame for the thumbnail: tightly packed BGRA, top row first.
struct Picture {
    uint32_t width = 0, height = 0;
    std::vector<uint8_t> bgra;
};

struct SaveRequest {
    std::string name, app, engine;
    std::string guest_package_sha256;
    uint32_t headphones = 0;
    Picture picture;
    // Writes the machine through the given writer, leaving the guest stopped; throws with the reason.
    std::function<void(const std::function<void(const void *, size_t)> &)> machine;
    // Restarts the guest if the save stopped it; called once whatever happened after machine began.
    std::function<void()> resume;
    // An estimate of the machine stream's size, for progress.
    uint64_t machine_estimate = 0;
};

void save(lt_device *, SaveRequest &, Observer &, lt_state_info *out);

// A state checked against the device it is to load into, its zip open.
struct Load {
    std::string id;
    Json manifest;
    int fd = -1;
    std::unique_ptr<zip::Reader> reader;
    uint32_t headphones = 0;
    ~Load();
};
// Refuses (LT_INVALID, with a sentence for the user) a state of another device, base, guest package, engine build or
// host kind, before anything is changed.
std::unique_ptr<Load> open(lt_device *, const std::string &id, const std::string &engine,
                           const std::string &guest_package_sha256);
// The saved storage in place of the device's (state/nand, nor.bin, sim, guest-verdicts): staged, then renamed in.
void restore_storage(Load &, lt_device *, Observer &);
// The machine stream, in order, to sink.
void read_machine(Load &, const std::function<void(const uint8_t *, size_t)> &sink, Observer &);

} // namespace lt::states
