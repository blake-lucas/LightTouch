#pragma once
#include "Json.hpp"
#include "Sha256.hpp"
#include "lighttouch.h"
#include <atomic>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
namespace lt {
namespace fs = std::filesystem;
struct Failure : std::runtime_error {
    lt_status status;
    Failure(lt_status s, const std::string &m) : std::runtime_error(m), status(s) {}
};
inline void require(bool b, const std::string &m, lt_status s = LT_INVALID) {
    if (!b)
        throw Failure(s, m);
}
inline void io(bool b, const std::string &m) { require(b, m + ": " + std::strerror(errno), LT_IO); }
template <class T> void abi(const T *p) {
    require(p && p->size >= sizeof(T) && p->version == LT_ABI_VERSION, "invalid ABI size/version");
}
inline lt_status error(lt_error *e, lt_status s, const std::string &m) {
    if (e && e->size >= sizeof(*e) && e->version == LT_ABI_VERSION) {
        e->code = s;
        snprintf(e->message, sizeof(e->message), "%s", m.c_str());
    }
    return s;
}
template <class F> lt_status guard(lt_error *e, F f) {
    try {
        f();
        return error(e, LT_OK, "");
    } catch (const Failure &x) {
        return error(e, x.status, x.what());
    } catch (const std::exception &x) {
        return error(e, LT_INVALID, x.what());
    }
}
std::string read_file(const fs::path &, size_t limit = 64u << 20);
std::string digest_file(const fs::path &);
struct AdmissionAssetCheck {
    fs::path path;
    std::string sha256, message;
};
std::unique_ptr<lt_device> open_device_checked(const lt_device_config *, lt_progress_fn, void *,
                                               const std::vector<AdmissionAssetCheck> &);
void atomic_write(const fs::path &, const std::string &);
// A synced, sparse copy of a regular file's bytes, as a new file (O_EXCL) of the app's own mode.
void copy_contents(const fs::path &, const fs::path &);
// A preparer's output folder moved in as a record's base (lt_library_adopt_prepared): validated against its own
// lock's digests (structure, listing and every file the lock names; nothing it names is read again) and flushed.
void adopt_base(const fs::path &, lt_import_phase_fn, void *);
// A new record's state, pinned to its base and holding the lock's digests, so its first start reads nothing either.
void seed_admission(const fs::path &base, const fs::path &state);
void sync_dir(const fs::path &);
std::string prepared_guest_package(lt_device *);
std::string guest_offer(lt_device *, const char *);
// A boot's verdict on the guest package it ran (GuestPackageJudge), for later offers.
void record_guest_verdict(lt_device *, bool good, int64_t serial);
// Whether a boot has called this device's guest package good, which takes its agent answering: its guest has one.
bool guest_agent_expected(lt_device *);
void clean_device(lt_device *, bool);
} // namespace lt
struct lt_device {
    std::filesystem::path base, state;
    int lease = -1;
    lt::Json lock;
    std::atomic<bool> dirty{false}, active{false};
    std::string base_digest;
    std::vector<std::string> args;
    std::vector<const char *> argv;
    ~lt_device();
};
