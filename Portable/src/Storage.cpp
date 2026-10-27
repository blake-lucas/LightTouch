#include "Internal.hpp"
#include <algorithm>
#include <array>
#include <fcntl.h>
#include <fstream>
#ifdef __linux__
#include <linux/fs.h>
#endif
#include <memory>
#include <map>
#include <mutex>
#include <set>
#include <thread>
#include <sstream>
#include "NandDisk.hpp"
#include "Os.hpp"
#include <functional>
#ifndef _WIN32
#include <sys/ioctl.h>
#include <sys/syscall.h>
#endif
#include <zlib.h>
#ifdef __APPLE__
#include <stdio.h>
#include <sys/clonefile.h>
#endif
namespace lt {
struct Fd {
    int n = -1;
    explicit Fd(int f) : n(f) {}
    ~Fd() {
        if (n >= 0)
            lt::os::close(n);
    }
    Fd(const Fd &) = delete;
};
static Fd open_read(const fs::path &p) {
    int fd = lt::os::open(p.c_str(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    io(fd >= 0, "open " + p.string());
    lt::os::Stat st {};
    io(lt::os::fstat(fd, &st) == 0, "fstat");
    require(S_ISREG(st.st_mode), "not a regular file");
    return Fd(fd);
}
static void write_all(int fd, const void *p, size_t n) {
    auto *b = static_cast<const char *>(p);
    while (n) {
        ssize_t k = lt::os::write(fd, b, n);
        if (k < 0 && errno == EINTR)
            continue;
        io(k > 0, "write");
        b += k;
        n -= size_t(k);
    }
}
static void read_all(int fd, void *p, size_t n) {
    auto *b = static_cast<char *>(p);
    while (n) {
        ssize_t k = lt::os::read(fd, b, n);
        if (k < 0 && errno == EINTR)
            continue;
        io(k > 0, "truncated input");
        b += k;
        n -= size_t(k);
    }
}
std::string read_file(const fs::path &p, size_t limit) {
    auto fd = open_read(p);
    lt::os::Stat st {};
    io(lt::os::fstat(fd.n, &st) == 0, "fstat");
    require(st.st_size >= 0 && uint64_t(st.st_size) <= limit, "file exceeds bound");
    std::string s(size_t(st.st_size), '\0');
    read_all(fd.n, s.data(), s.size());
    return s;
}
struct AdmissionProgress {
    lt_progress_fn callback;
    void *opaque;
    uint64_t done = 0, total = 0;
    // Set while several threads hash: the callback then runs under it, one call at a time, and
    // once one thread fails or is cancelled the others stop at their next step.
    std::mutex *shared = nullptr;
    bool failed = false;
    void checkpoint() const {
        if (!shared)
            return notify();
        std::lock_guard<std::mutex> lock(*shared);
        notify();
    }
    void advance(uint64_t bytes) {
        std::unique_lock<std::mutex> lock;
        if (shared)
            lock = std::unique_lock<std::mutex>(*shared);
        require(done <= total && bytes <= total - done, "admission byte count exceeds checked plan");
        done += bytes;
        notify();
    }

  private:
    void notify() const {
        require(!failed, "device admission stopped", LT_CANCELLED);
        require(!callback || callback(opaque, done, total) == 0, "device admission cancelled", LT_CANCELLED);
    }
};
struct ImportObservation {
    lt_import_phase_fn callback = nullptr;
    void *opaque = nullptr;
    const char *phase = "hashing";
    uint64_t done = 0, total = 0;
    void checkpoint() const {
        require(!callback || callback(opaque, phase, done, total) == 0, "import cancelled", LT_CANCELLED);
    }
    void begin(const char *name, uint64_t bytes = 0) {
        phase = name;
        done = 0;
        total = bytes;
        checkpoint();
    }
    static int notify(void *opaque, uint64_t done, uint64_t total) {
        auto &self = *static_cast<ImportObservation *>(opaque);
        self.done = done;
        self.total = total;
        return self.callback ? self.callback(self.opaque, self.phase, done, total) : 0;
    }
};
static std::string digest_fd(int fd, AdmissionProgress *progress = nullptr, uint64_t planned_bytes = 0) {
    Sha256 hash;
    std::array<char, 65536> b{};
    uint64_t consumed = 0;
    if (progress) {
        lt::os::Stat st {};
        io(lt::os::fstat(fd, &st) == 0, "fstat admission asset");
        require(st.st_size >= 0 && uint64_t(st.st_size) == planned_bytes, "admission asset size changed");
        progress->checkpoint();
    }
    for (;;) {
        ssize_t n = lt::os::read(fd, b.data(), b.size());
        if (n < 0 && errno == EINTR)
            continue;
        io(n >= 0, "hash read");
        if (!n)
            break;
        if (progress)
            require(consumed <= planned_bytes && uint64_t(n) <= planned_bytes - consumed,
                    "admission asset grew while hashing");
        hash.update(b.data(), size_t(n));
        if (progress) {
            consumed += uint64_t(n);
            progress->advance(uint64_t(n));
        }
    }
    if (progress)
        require(consumed == planned_bytes, "admission asset truncated while hashing");
    return hash.finish();
}
std::string digest_file(const fs::path &p) {
    auto fd = open_read(p);
    return digest_fd(fd.n);
}
void sync_dir(const fs::path &p) {
    Fd fd(lt::os::open(p.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW));
    io(fd.n >= 0, "open directory");
    io(lt::os::fsync(fd.n) == 0, "sync directory");
}
void atomic_write(const fs::path &p, const std::string &s) {
    std::string tmp = (p.parent_path() / ".lt-write-XXXXXX").string();
    std::vector<char> b(tmp.begin(), tmp.end());
    b.push_back(0);
    Fd fd(lt::os::mkstemp(b.data()));
    io(fd.n >= 0, "create metadata");
    try {
        write_all(fd.n, s.data(), s.size());
        io(lt::os::fsync(fd.n) == 0, "sync metadata");
        io(lt::os::rename(b.data(), p.c_str()) == 0, "publish metadata");
        sync_dir(p.parent_path());
    } catch (...) {
        lt::os::unlink(b.data());
        throw;
    }
}
static void safe_name(const std::string &s) {
    require(!s.empty() && s.size() < 4096 && s.front() != '/' && s.find('\\') == std::string::npos &&
                s.find('\0') == std::string::npos && s.find_first_of("\n\r\t") == std::string::npos,
            "unsafe archive name");
    size_t start = 0;
    for (;;) {
        size_t end = s.find('/', start);
        std::string c = s.substr(start, end - start);
        require(!c.empty() && c != "." && c != "..", "unsafe archive component");
        if (end == std::string::npos)
            break;
        start = end + 1;
    }
}
static bool contained(const fs::path &a, const fs::path &b) {
    auto ia = a.begin(), ib = b.begin();
    for (; ia != a.end() && ib != b.end(); ++ia, ++ib)
        if (*ia != *ib)
            return false;
    return ia == a.end();
}
static bool sha256_text(const Json &value) {
    const auto text = value.string();
    return text.size() == 64 && text.find_first_not_of("0123456789abcdef") == std::string::npos;
}
static uint32_t little32(const unsigned char *p) {
    return uint32_t(p[0]) | (uint32_t(p[1]) << 8) | (uint32_t(p[2]) << 16) | (uint32_t(p[3]) << 24);
}
static void validate_packed_nand(const fs::path &path, uint64_t expected_pages, AdmissionProgress &events) {
    auto fd = open_read(path);
    lt::os::Stat st {};
    io(lt::os::fstat(fd.n, &st) == 0, "stat packed NAND");
    require(st.st_size >= 24, "truncated packed NAND header");
    std::array<unsigned char, 24> header{};
    read_all(fd.n, header.data(), header.size());
    require(memcmp(header.data(), "ITNAND01", 8) == 0, "invalid packed NAND magic");
    require(little32(header.data() + 8) == 4096 && little32(header.data() + 12) == 64,
            "unsupported packed NAND page geometry");
    const uint64_t chips = little32(header.data() + 16), pages = little32(header.data() + 20);
    // The shared n72 importer emits the full four-chip FMSS address space.
    require(chips == 4 && pages == 4096 * 128,
            "unsupported packed NAND address geometry");
    const uint64_t entries = chips * pages, index_bytes = entries * 4;
    require(index_bytes <= uint64_t(st.st_size) - header.size(), "truncated packed NAND index");
    const uint64_t record_bytes = uint64_t(st.st_size) - header.size() - index_bytes;
    require(record_bytes % 4160 == 0, "truncated packed NAND record");
    const uint64_t records = record_bytes / 4160;
    require(records <= entries && records == expected_pages, "packed NAND page count mismatch");
    std::array<unsigned char, 65536> index{};
    std::vector<unsigned char> seen(size_t((records + 7) / 8), 0);
    uint64_t referenced = 0;
    uint64_t remaining = index_bytes;
    while (remaining) {
        events.checkpoint();
        const size_t count = size_t(std::min<uint64_t>(remaining, index.size()));
        read_all(fd.n, index.data(), count);
        for (size_t offset = 0; offset < count; offset += 4) {
            const uint64_t slot = little32(index.data() + offset);
            require(slot <= records, "packed NAND index record out of bounds");
            if (slot) {
                const unsigned char mask = 1u << ((slot - 1) % 8);
                auto &byte = seen[size_t((slot - 1) / 8)];
                require(!(byte & mask), "duplicate packed NAND index record");
                byte |= mask;
                ++referenced;
            }
        }
        remaining -= count;
    }
    require(referenced == records, "unreferenced packed NAND record");
    events.checkpoint();
}
// A file keeps the digest it was verified with while it keeps its identity: device, inode, size and
// modification and change times (every write, rename onto it or chmod moves the change time).
using DigestCache = std::map<std::string, std::string>;
static std::string file_identity(const fs::path &path, const lt::os::Stat &st) {
#if defined(__APPLE__) && !defined(_WIN32)
    const struct timespec &mtime = st.st_mtimespec, &ctime = st.st_ctimespec;
#else
    const struct timespec &mtime = st.st_mtim, &ctime = st.st_ctim;
#endif
    return path.string() + "|" + std::to_string(st.st_dev) + "|" + std::to_string(st.st_ino) + "|" +
           std::to_string(st.st_size) + "|" + std::to_string(mtime.tv_sec) + "." + std::to_string(mtime.tv_nsec) +
           "|" + std::to_string(ctime.tv_sec) + "." + std::to_string(ctime.tv_nsec);
}
static DigestCache read_digest_cache(const fs::path &path) {
    DigestCache cache;
    std::error_code ec;
    if (!fs::is_regular_file(fs::symlink_status(path, ec)))
        return cache;
    std::istringstream lines(read_file(path, 1 << 20));
    for (std::string line; std::getline(lines, line);) {
        const auto space = line.rfind(' ');
        if (space != std::string::npos && line.size() - space - 1 == 64)
            cache[line.substr(0, space)] = line.substr(space + 1);
    }
    return cache;
}
static std::string dump_digest_cache(const DigestCache &cache) {
    std::string text;
    for (const auto &entry : cache)
        text += entry.first + " " + entry.second + "\n";
    return text;
}
// How each board firmwarekit prepares boots (Light Touch for Mac's BootRecipe and Board.requiredFiles): the
// S5L8900 (iPod touch 1G, original iPhone) and S5L8720 (iPod touch 2G) from their SecureROM, which the caller
// supplies; the S5L8920/S5L8922 (iPhone 3GS, iPod touch 3G) and A4 boards from prepared files alone. The engine
// names the -M machine.
enum class BootFamily { S5L8900, S5L8720, A4 };
struct BootRoute {
    BootFamily family;
    std::string strategy;
    bool modem;  // baseband=on: the iPhone 3GS and iPhone 4 (the original iPhone's machine has its modem built in)
    std::vector<std::pair<std::string, std::string>> outputs;  // the lock outputs (key, file) the boot reads
    bool needs_bootrom() const { return family != BootFamily::A4; }
};
static BootRoute boot_route(const std::string &board, const Json &lock) {
    static const std::map<std::string, std::pair<BootFamily, bool>> boards = {
        {"n45ap", {BootFamily::S5L8900, false}}, {"m68ap", {BootFamily::S5L8900, false}},
        {"n72ap", {BootFamily::S5L8720, false}}, {"n18ap", {BootFamily::A4, false}},
        {"n88ap", {BootFamily::A4, true}},       {"k48ap", {BootFamily::A4, false}},
        {"n81ap", {BootFamily::A4, false}},      {"n90ap", {BootFamily::A4, true}}};
    const auto b = boards.find(board);
    require(b != boards.end(), "no boot route for board " + board, LT_UNSUPPORTED);
    BootRoute r{b->second.first, "", b->second.second, {}};
    const Json *strategy = lock.find("boot_strategy");
    r.strategy = strategy && strategy->kind == Json::String ? strategy->string()
                                                             : r.family == BootFamily::A4 ? "kboot" : "iboot";
    const std::pair<std::string, std::string> iboot{"iboot", "iBoot.bin"}, nor{"nor", "nor.bin"},
        gid{"gid_blobs", "gid-blobs.bin"}, kboot{"kboot", "kboot.bin"};
    if (r.family == BootFamily::S5L8900 && r.strategy == "iboot")
        r.outputs = {iboot, nor};
    else if (r.family == BootFamily::S5L8720 && r.strategy == "iboot")
        r.outputs = {iboot, nor, gid};
    else if (r.family == BootFamily::S5L8720 && r.strategy == "bootrom")
        r.outputs = {nor, gid};
    else if (r.family == BootFamily::A4 && r.strategy == "kboot")
        r.outputs = {kboot};  // and nor.bin when the device has one (4.x data protection, the S5L8920's own)
    else if (board == "k48ap" && r.strategy == "iboot")
        r.outputs = {iboot, nor, gid};
    else
        require(false, "unsupported boot strategy " + r.strategy + " for " + board, LT_UNSUPPORTED);
    return r;
}
// With [cache], files whose identity matches an entry reuse its digest instead of being read again;
// [cache] is then replaced by the identities and digests of this admission.
static void validate_base(const fs::path &base, AdmissionProgress *progress = nullptr,
                          const std::vector<AdmissionAssetCheck> &extra_checks = {}, DigestCache *cache = nullptr) {
    Json j = parse(read_file(base / "device.lock.json"));
    require(j.at("format").integer() == 1, "unsupported device lock schema", LT_UNSUPPORTED);
    require(!j.at("board").string().empty() && !j.at("build").string().empty(), "missing board/build");
    const Json &outputs = j.at("outputs");
    require(outputs.kind == Json::Object, "invalid outputs");
    const auto &nand = outputs.at("nand");
    const std::string board = j.at("board").string();
    const bool packed_nand = nand.find("format");
    require(nand.at("path").string() == "nand", "invalid NAND output path");
    if (packed_nand) {
        require(board == "n72ap" && nand.at("format").string() == "ITNAND01",
                "packed NAND is supported only for n72 ITNAND01", LT_UNSUPPORTED);
        require(!nand.find("listing_sha256") && sha256_text(nand.at("sha256")) &&
                    sha256_text(nand.at("source_listing_sha256")),
                "missing authoritative packed NAND integrity record");
        nand.at("pages").integer();
    } else
        require(nand.at("listing_sha256").string().size() == 64,
                "missing authoritative NAND integrity record");
    auto required_file = [&](const std::string &key, const std::string &name) {
        const auto &r = outputs.at(key);
        require(r.at("path").string() == name && r.at("sha256").string().size() == 64,
                "missing authoritative " + name + " integrity record");
    };
    for (const auto &output : boot_route(board, j).outputs)
        required_file(output.first, output.second);
    // Enumerate once, then hash a stable, sorted, checked file plan. Listing
    // strings and state/lock metadata are not source asset digest input bytes.
    AdmissionProgress quiet{nullptr, nullptr};
    auto &events = progress ? *progress : quiet;
    std::map<fs::path, uint64_t> assets;
    std::map<fs::path, std::string> identities;
    std::vector<AdmissionAssetCheck> checks = extra_checks;
    struct ListingCheck {
        std::vector<std::pair<std::string, fs::path>> files;
        std::string expected;
        uint64_t pages = 0;
        bool check_pages = false;
    };
    std::vector<ListingCheck> listings;
    auto add_asset = [&](const fs::path &path) {
        events.checkpoint();
        lt::os::Stat st {};
        io(lt::os::lstat(path.c_str(), &st) == 0, "stat admission asset");
        require(S_ISREG(st.st_mode) && st.st_size >= 0, "admission asset must be a regular file");
        assets.emplace(path, uint64_t(st.st_size));
        identities.emplace(path, file_identity(path, st));
    };
    for (const auto &check : extra_checks)
        add_asset(check.path);
    for (const auto &pair : outputs.object) {
        events.checkpoint();
        const Json &o = pair.second;
        if (o.kind == Json::Null)
            continue;  // FirmwareKit records an output its board leaves out as null (a 3.x kboot device's nor.bin)
        const Json *path = o.find("path");
        require(path, "output path missing");
        std::string name = path->string();
        safe_name(name);
        if (const Json *h = o.find("sha256")) {
            add_asset(base / name);
            checks.push_back({base / name, h->string(), "output checksum mismatch: " + name});
        }
        if (const Json *h = o.find("listing_sha256")) {
            require(fs::is_directory(fs::symlink_status(base / name)), "NAND is not a directory");
            std::vector<std::string> files;
            for (const auto &e : fs::recursive_directory_iterator(base / name)) {
                events.checkpoint();
                require(!e.is_symlink(), "symlink in NAND");
                if (e.is_regular_file())
                    files.push_back(e.path().lexically_relative(base / name).generic_string());
                else
                    require(e.is_directory(), "special file in NAND");
            }
            std::sort(files.begin(), files.end());
            ListingCheck listing;
            listing.expected = h->string();
            for (const auto &file : files) {
                safe_name(file);
                add_asset(base / name / file);
                listing.files.emplace_back(file, base / name / file);
            }
            if (const Json *p = o.find("pages")) {
                listing.pages = p->integer();
                listing.check_pages = true;
            }
            listings.push_back(std::move(listing));
        }
    }
    if (const Json *i = j.find("identity"))
        if (const Json *h = i->find("sha256")) {
            add_asset(base / "identity.json");
            checks.push_back({base / "identity.json", h->string(), "identity checksum mismatch"});
        }
    // Progress counts only the bytes this admission reads.
    auto known_digest = [&](const fs::path &path) {
        return cache ? cache->find(identities.at(path)) : DigestCache::iterator{};
    };
    uint64_t total = 0;
    for (const auto &asset : assets) {
        if (cache && known_digest(asset.first) != cache->end())
            continue;
        require(asset.second <= UINT64_MAX - total, "admission byte count overflow");
        total += asset.second;
    }
    events.total = total;
    events.checkpoint();
    std::map<fs::path, std::string> digests;
    DigestCache verified;
    std::vector<std::pair<fs::path, uint64_t>> work;
    for (const auto &asset : assets) {
        events.checkpoint();
        const auto known = known_digest(asset.first);
        if (cache && known != cache->end()) {
            digests.emplace(asset.first, known->second);
            verified.emplace(*known);
        } else
            work.push_back(asset);
    }
    // Largest first, spread over a few threads: an iPod touch 4G NAND is eight 2 GB files.
    std::stable_sort(work.begin(), work.end(), [](const auto &a, const auto &b) { return a.second > b.second; });
    std::mutex guard;
    size_t next = 0;
    std::exception_ptr failure;
    auto hash_files = [&] {
        for (;;) {
            std::pair<fs::path, uint64_t> asset;
            {
                std::lock_guard<std::mutex> lock(guard);
                if (failure || next == work.size())
                    return;
                asset = work[next++];
            }
            try {
                auto fd = open_read(asset.first);
                // Identify the file as opened: a change during hashing moves its change time, so
                // the next admission reads it again.
                lt::os::Stat st {};
                io(lt::os::fstat(fd.n, &st) == 0, "fstat admission asset");
                const auto digest = digest_fd(fd.n, &events, asset.second);
                std::lock_guard<std::mutex> lock(guard);
                digests.emplace(asset.first, digest);
                verified.emplace(file_identity(asset.first, st), digest);
            } catch (...) {
                std::lock_guard<std::mutex> lock(guard);
                if (!failure)
                    failure = std::current_exception();
                events.failed = true;
                return;
            }
        }
    };
    const size_t threads =
        std::min<size_t>({work.size(), 4, std::max(1u, std::thread::hardware_concurrency())});
    if (threads > 1) {
        // The progress lock is the work lock: one mutex orders every callback and result.
        events.shared = &guard;
        std::vector<std::thread> pool;
        for (size_t i = 1; i < threads; i++)
            pool.emplace_back(hash_files);
        hash_files();
        for (auto &thread : pool)
            thread.join();
        events.shared = nullptr;
    } else
        hash_files();
    if (failure)
        std::rethrow_exception(failure);
    for (const auto &check : checks) {
        events.checkpoint();
        require(digests.at(check.path) == check.sha256, check.message);
    }
    if (packed_nand)
        validate_packed_nand(base / "nand", nand.at("pages").integer(), events);
    for (const auto &check : listings) {
        events.checkpoint();
        Sha256 listing;
        for (const auto &file : check.files) {
            events.checkpoint();
            std::string line = file.first + " " + digests.at(file.second) + "\n";
            listing.update(line.data(), line.size());
        }
        require(listing.finish() == check.expected, "NAND listing checksum mismatch");
        if (check.check_pages)
            require(check.files.size() == check.pages, "NAND page count mismatch");
    }
    events.checkpoint();
    if (cache)
        *cache = std::move(verified);
}
struct Stage {
    fs::path path, destination;
    bool published = false;
    bool bulk_sync = false;
    bool cleanup_on_failure = true, flush_existing_files = false;
    // Files this import wrote and hashed itself (a NAND container), by identity: verification takes them as read.
    DigestCache known;
    explicit Stage(const char *dest, bool archive = false) {
#if defined(__linux__) && !defined(__ANDROID__) && defined(SYS_syncfs)
        // An archive may contain over 100,000 tiny NAND files. Flush the
        // unpublished transaction once rather than serializing every file's
        // write behind a separate storage barrier. Android retains its existing
        // per-file policy; syncfs can be restricted by its sandbox.
        bulk_sync = archive;
#else
        (void)archive;
#endif
        require(dest && fs::path(dest).is_absolute(), "destination must be absolute");
        destination = fs::path(dest).lexically_normal();
        require(!fs::exists(fs::symlink_status(destination)), "destination already exists");
        auto parent = fs::canonical(destination.parent_path());
        destination = parent / destination.filename();
        std::string pattern = (parent / ".lt-import-XXXXXX").string();
        std::vector<char> b(pattern.begin(), pattern.end());
        b.push_back(0);
        io(lt::os::mkdtemp(b.data()) != nullptr, "create staging");
        path = b.data();
    }
    Stage(const char *source, const char *dest) {
        cleanup_on_failure = false;
        flush_existing_files = true;
        require(source && dest && fs::path(source).is_absolute() && fs::path(dest).is_absolute(),
                "stage and destination must be absolute");
        const auto input = fs::path(source).lexically_normal();
        const auto output = fs::path(dest).lexically_normal();
        require(input.filename() != "." && !input.filename().empty() && output.filename() != "." &&
                    !output.filename().empty(), "invalid stage or destination name");
        const auto parent = fs::canonical(output.parent_path());
        require(fs::canonical(input.parent_path()) == parent && input.filename() != output.filename(),
                "import stage must be a distinct destination sibling");
        path = parent / input.filename();
        destination = parent / output.filename();
        require(!fs::exists(fs::symlink_status(destination)), "destination already exists");
        lt::os::Stat st {};
        io(lt::os::lstat(parent.c_str(), &st) == 0, "stat import parent");
        require(S_ISDIR(st.st_mode) && st.st_uid == lt::os::euid() && !(st.st_mode & 0022),
                "import parent must be owned with no external writers");
        io(lt::os::lstat(path.c_str(), &st) == 0, "stat import stage");
        require(S_ISDIR(st.st_mode) && st.st_uid == lt::os::euid() && (st.st_mode & 0777) == 0700,
                "import stage must be an owned private 0700 directory");
        // Reject aliases before integrity checking.
        for (const auto &entry : fs::recursive_directory_iterator(path)) {
            io(lt::os::lstat(entry.path().c_str(), &st) == 0, "stat import stage entry");
            require(st.st_uid == lt::os::euid() && !(st.st_mode & 0022) &&
                        (S_ISDIR(st.st_mode) || (S_ISREG(st.st_mode) && st.st_nlink == 1)),
                    "import stage entry must be owned and unaliased with no external writers");
        }
#if defined(__linux__) && !defined(__ANDROID__) && defined(SYS_syncfs)
        bulk_sync = true;
#endif
    }
    ~Stage() {
        if (!published && cleanup_on_failure) {
            std::error_code ec;
            fs::remove_all(path, ec);
        }
    }
    void finish(ImportObservation *observation = nullptr) {
        AdmissionProgress verify{ImportObservation::notify, observation};
        if (observation)
            observation->begin("verifying");
        validate_base(path, observation ? &verify : nullptr, {}, known.empty() ? nullptr : &known);
        std::vector<fs::path> dirs;
        for (const auto &e : fs::recursive_directory_iterator(path)) {
            if (observation)
                observation->checkpoint();
            if (e.is_directory())
                dirs.push_back(e.path());
            else {
                if (flush_existing_files && !bulk_sync) {
                    auto file = open_read(e.path());
                    io(lt::os::fsync(file.n) == 0, "sync staged import file");
                }
            }
        }
        for (auto it = dirs.rbegin(); it != dirs.rend(); ++it) {
            if (observation)
                observation->checkpoint();
            if (!bulk_sync)
                sync_dir(*it);
        }
        if (!bulk_sync)
            sync_dir(path);
        if (observation)
            observation->checkpoint();
#if defined(__linux__) && !defined(__ANDROID__) && defined(SYS_syncfs)
        if (bulk_sync) {
            Fd stage_fd(lt::os::open(path.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW));
            io(stage_fd.n >= 0, "open unpublished import for sync");
            // All staged data and directory entries must be
            // durable BEFORE rename publishes the complete base. This flushes
            // the containing filesystem, so unrelated dirty writes can add time.
            if (syscall(SYS_syncfs, stage_fd.n) != 0) {
                const int failure = errno;
                if (failure != ENOSYS && failure != EPERM && failure != EINVAL)
                    io(false, "sync imported filesystem");
                // Unsupported/restricted filesystem: retain the old durability
                // guarantee by opening and flushing each completed file.
                for (const auto &entry : fs::recursive_directory_iterator(path)) {
                    if (observation)
                        observation->checkpoint();
                    if (entry.is_regular_file()) {
                        auto file = open_read(entry.path());
                        io(lt::os::fsync(file.n) == 0, "sync extracted file fallback");
                    }
                }
                for (auto it = dirs.rbegin(); it != dirs.rend(); ++it)
                    sync_dir(*it);
                sync_dir(path);
            }
            if (observation)
                observation->checkpoint();
        }
#endif
#ifdef SYS_renameat2
        io(syscall(SYS_renameat2, AT_FDCWD, path.c_str(), AT_FDCWD, destination.c_str(), RENAME_NOREPLACE) == 0,
           "publish base");
#elif defined(__APPLE__)
        io(renamex_np(path.c_str(), destination.c_str(), RENAME_EXCL) == 0, "publish base");
#elif defined(_WIN32)
        io(lt::os::rename_noreplace(path.c_str(), destination.c_str()) == 0, "publish base");
#else
        require(!fs::exists(destination), "destination race");
        io(lt::os::rename(path.c_str(), destination.c_str()) == 0, "publish base");
#endif
        published = true;
        sync_dir(destination.parent_path());
    }
};
// The digests a lock records for the base files it names (FirmwareKit writes outputs.nand.files besides each
// output's sha256), keyed by identity as validate_base's cache is: a base fresh from the preparer is admitted
// without reading it again. Files the lock does not name are left out and read as usual.
static DigestCache lock_digests(const fs::path &base) {
    DigestCache cache;
    Json j = parse(read_file(base / "device.lock.json"));
    auto add = [&](const fs::path &path, const Json &sha) {
        lt::os::Stat st {};
        if (sha.kind == Json::String && sha.string().size() == 64 && lt::os::lstat(path.c_str(), &st) == 0 &&
            S_ISREG(st.st_mode))
            cache[file_identity(path, st)] = sha.string();
    };
    for (const auto &pair : j.at("outputs").object) {
        const Json *path = pair.second.find("path");
        if (!path)
            continue;
        std::string name = path->string();
        safe_name(name);
        if (const Json *h = pair.second.find("sha256"))
            add(base / name, *h);
        if (const Json *files = pair.second.find("files"); files && files->kind == Json::Object)
            for (const auto &file : files->object) {
                safe_name(file.first);
                add(base / name / file.first, file.second);
            }
    }
    if (const Json *i = j.find("identity"))
        if (const Json *h = i->find("sha256"))
            add(base / "identity.json", *h);
    return cache;
}
void adopt_base(const fs::path &base, lt_import_phase_fn observer, void *opaque) {
    ImportObservation observation{observer, opaque};
    observation.begin("verifying");
    AdmissionProgress verify{ImportObservation::notify, &observation};
    DigestCache cache = lock_digests(base);
    validate_base(base, &verify, {}, &cache);
    std::vector<fs::path> dirs{base};
    for (const auto &e : fs::recursive_directory_iterator(base)) {
        observation.checkpoint();
        require(!e.is_symlink(), "base symlink refused");
        if (e.is_directory()) {
            dirs.push_back(e.path());
            continue;
        }
        require(e.is_regular_file(), "special base file refused");
        auto file = open_read(e.path());
        io(lt::os::fsync(file.n) == 0, "sync adopted base file");
    }
    for (auto it = dirs.rbegin(); it != dirs.rend(); ++it)
        sync_dir(*it);
}
void seed_admission(const fs::path &base_path, const fs::path &state) {
    const auto base = fs::canonical(base_path);  // as open_device_checked names the files
    DigestCache cache = lock_digests(base);
    validate_base(base, nullptr, {}, &cache);
    atomic_write(state / ".base-identity", digest_file(base / "device.lock.json"));
    atomic_write(state / ".verified-digests", dump_digest_cache(cache));
}
static void sparse_write(int fd, const uint8_t *bytes, size_t size) {
    if (std::all_of(bytes, bytes + size, [](uint8_t value) { return value == 0; }))
        io(lt::os::lseek(fd, int64_t(size), SEEK_CUR) >= 0, "seek zero archive span");
    else
        write_all(fd, bytes, size);
}
// n72 NAND arrives as ~110,000 4160-byte page files, nand/csN/N.page. Extracted one file each (with a
// sync per file on Android) that took minutes; the importer streams them instead into the record's
// NAND container (the Pages route below); packed_page names a page's slot, chip x 524288 + page.
constexpr uint32_t packed_pages_per_cs = 524288, packed_record = 4160;
static bool packed_page(const std::string &name, uint32_t &slot) {
    if (name.size() < 15 || name.compare(0, 7, "nand/cs") || name[7] < '0' || name[7] > '3' || name[8] != '/' ||
        name.compare(name.size() - 5, 5, ".page"))
        return false;
    const std::string digits = name.substr(9, name.size() - 14);
    if (digits.empty() || digits.size() > 7 || digits.find_first_not_of("0123456789") != std::string::npos ||
        (digits.size() > 1 && digits[0] == '0'))
        return false;
    const uint32_t page = uint32_t(std::stoul(digits));
    slot = uint32_t(name[7] - '0') * packed_pages_per_cs + page;
    return page < packed_pages_per_cs;
}
static uint64_t limit(const lt_import_config *c) {
    return c->max_bytes ? std::min<uint64_t>(c->max_bytes, 64ull << 30) : 64ull << 30;
}
static void progress(const lt_import_config *c, uint64_t written, uint64_t total) {
    if (c->progress)
        require(c->progress(c->opaque, written, total) == 0, "import cancelled", LT_CANCELLED);
}
#ifdef _WIN32
// Where a file's data is, as SEEK_DATA/SEEK_HOLE say elsewhere: a hole reads as zeros, so a NAND chip (2 GB, a few
// hundred MB written) is copied without reading its holes. All of it where the filesystem can't say (FAT).
static std::vector<std::pair<int64_t, int64_t>> allocated_ranges(int fd, int64_t size) {
    std::vector<std::pair<int64_t, int64_t>> ranges;
    FILE_ALLOCATED_RANGE_BUFFER query {}, found[256];
    query.Length.QuadPart = size;
    while (query.Length.QuadPart > 0) {
        DWORD bytes = 0;
        BOOL done = DeviceIoControl(lt::os::handle(fd), FSCTL_QUERY_ALLOCATED_RANGES, &query, sizeof query, found,
                                    sizeof found, &bytes, nullptr);
        if (!done && GetLastError() != ERROR_MORE_DATA)
            return {{0, size}};
        const DWORD n = bytes / sizeof found[0];
        for (DWORD i = 0; i < n; ++i)
            ranges.emplace_back(found[i].FileOffset.QuadPart, found[i].FileOffset.QuadPart + found[i].Length.QuadPart);
        if (done || n == 0)
            break;
        query.FileOffset.QuadPart = ranges.back().second;
        query.Length.QuadPart = size - ranges.back().second;
    }
    return ranges;
}
#endif
static void clone_sparse(const fs::path &from, const fs::path &to) {
    auto in = open_read(from);
    lt::os::Stat st {};
    io(lt::os::fstat(in.n, &st) == 0, "fstat");
#ifdef __APPLE__
    // APFS copies on write; the clone keeps holes. Falls through to copying on other filesystems.
    if (fclonefileat(in.n, AT_FDCWD, to.c_str(), CLONE_NOFOLLOW) == 0) {
        lt::os::chmod(to.c_str(), 0600);  // a clone keeps its source's mode; a copy is the device's to write
        return;
    }
#endif
    Fd out(lt::os::open(to.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600));
    io(out.n >= 0, "create " + to.string());
#ifdef FICLONE
    if (ioctl(out.n, FICLONE, in.n) == 0)
        return;
#endif
    io(lt::os::ftruncate(out.n, st.st_size) == 0, "size " + to.string());
    std::vector<uint8_t> buffer(1 << 20);
#ifdef _WIN32
    const auto ranges = allocated_ranges(in.n, st.st_size);
    for (auto [at, end] : ranges) {
#else
    int64_t at = 0;
    while (at < st.st_size) {
        int64_t end = st.st_size;
#ifdef SEEK_DATA
        int64_t data = lt::os::lseek(in.n, at, SEEK_DATA);
        if (data < 0)
            break;  // only a hole remains
        at = data;
        end = lt::os::lseek(in.n, data, SEEK_HOLE);
        io(end > 0, "seek hole " + from.string());
#endif
#endif
        while (at < end) {
            const size_t want = size_t(std::min<int64_t>(int64_t(buffer.size()), end - at));
            ssize_t n = lt::os::pread(in.n, buffer.data(), want, at);
            io(n > 0, "read " + from.string());
            if (!std::all_of(buffer.begin(), buffer.begin() + n, [](uint8_t v) { return v == 0; })) {
                for (ssize_t done = 0; done < n;) {
                    ssize_t k = lt::os::pwrite(out.n, buffer.data() + done, size_t(n - done), at + done);
                    io(k > 0, "write " + to.string());
                    done += k;
                }
            }
            at += n;
        }
    }
    io(lt::os::fsync(out.n) == 0, "sync " + to.string());
}
// Sparse: a prepared folder's NAND chips are mostly holes (an n81ap store is 17.7 GB, ~1.8 GB of data), and a
// dense copy wrote all of it. The copy is a new file of this app's (0600), whatever the source's mode.
void copy_contents(const fs::path &src, const fs::path &dst) { clone_sparse(src, dst); }
// Where a file's data is: [start, end) ranges in order (a hole reads as zeros). All of it where the filesystem can't
// say.
static std::vector<std::pair<int64_t, int64_t>> data_ranges(int fd, int64_t size) {
#ifdef _WIN32
    return allocated_ranges(fd, size);
#else
    std::vector<std::pair<int64_t, int64_t>> ranges;
#ifdef SEEK_DATA
    for (int64_t at = 0; at < size;) {
        const int64_t start = lt::os::lseek(fd, at, SEEK_DATA);
        if (start < 0)
            break;  // only a hole remains (or no answer: ENXIO is the usual end)
        const int64_t end = lt::os::lseek(fd, start, SEEK_HOLE);
        io(end > start, "seek hole");
        ranges.emplace_back(start, std::min(end, size));
        at = end;
    }
#else
    ranges.emplace_back(0, size);
#endif
    return ranges;
#endif
}
static void hash_zeros(Sha256 &hash, uint64_t n) {
    static const std::vector<uint8_t> zeros(1 << 20, 0);
    while (n) {
        const size_t k = size_t(std::min<uint64_t>(n, zeros.size()));
        hash.update(zeros.data(), k);
        n -= k;
    }
}
// A NAND store arriving in an import becomes the record's container (NandDisk.hpp) as it comes: its chip files' bytes,
// or the iPod touch 2G's page files, go into stage/nand/nand.qcow2 and nowhere else. The source lock's NAND listing is
// checked against what arrived; the lock then names the container (the source's stays beside it as
// source-device.lock.json).
struct ContainerImport {
    fs::path stage, nand;
    nand_disk::Layout layout;
    std::unique_ptr<nand_disk::Writer> writer;
    std::string geometry;                         // the source's geometry.json
    std::map<std::string, std::string> digests;   // each source NAND file as its listing names it -> sha256
    std::vector<std::pair<fs::path, size_t>> staged;  // chip files extracted before geometry.json came, and their chip
    explicit ContainerImport(const fs::path &stage_path) : stage(stage_path), nand(stage_path / "nand") {}
    bool begun() const { return bool(writer); }
    void begin(const std::string &geometry_text) {
        geometry = geometry_text;
        layout = nand_disk::Layout::of(parse(geometry));
        fs::create_directories(nand);
        writer = std::make_unique<nand_disk::Writer>(nand / nand_disk::file_name, layout.size());
    }
    // The chip a file of the store holds, by its name; -1 when it is not one.
    static long chip_of(const nand_disk::Layout &layout, const std::string &name) {
        const auto it = std::find(layout.names.begin(), layout.names.end(), name);
        return it == layout.names.end() ? -1 : long(it - layout.names.begin());
    }
    // `n` bytes of chip `chip` at its file's offset `at`: split at erase blocks, each of which sits at its own place.
    void put(size_t chip, uint64_t at, const uint8_t *p, size_t n) {
        while (n) {
            const size_t take = size_t(std::min<uint64_t>(n, layout.block_bytes() - at % layout.block_bytes()));
            writer->put(layout.place(chip, at), p, take);
            at += take;
            p += take;
            n -= take;
        }
    }
    // One 2G page record, as its page file holds it.
    void put_n72(uint32_t cs, uint32_t page, const uint8_t *record) {
        std::array<uint8_t, nand_disk::n72_record> r{};
        memcpy(r.data(), record, r.size());
        nand_disk::n72_encode(r.data());
        put(cs, uint64_t(page) * r.size(), r.data(), r.size());
    }
    // A chip file on disk: its data ranges put, and its digest (holes hash as the zeros they read as). `step` hears
    // the file's bytes as they are covered.
    std::string put_file(const fs::path &file, size_t chip, const std::function<void(uint64_t)> &step) {
        auto in = open_read(file);
        lt::os::Stat st {};
        io(lt::os::fstat(in.n, &st) == 0, "stat NAND chip file");
        require(uint64_t(st.st_size) == layout.chip_bytes(), file.filename().string() + ": not the geometry's chip size");
        Sha256 hash;
        std::vector<uint8_t> buffer(1 << 20);
        int64_t at = 0;
        for (auto [start, end] : data_ranges(in.n, st.st_size)) {
            hash_zeros(hash, uint64_t(start - at));
            step(uint64_t(start - at));
            for (at = start; at < end;) {
                const size_t want = size_t(std::min<int64_t>(int64_t(buffer.size()), end - at));
                ssize_t n = lt::os::pread(in.n, buffer.data(), want, at);
                if (n < 0 && errno == EINTR)
                    continue;
                io(n > 0, "read " + file.string());
                hash.update(buffer.data(), size_t(n));
                put(chip, uint64_t(at), buffer.data(), size_t(n));
                at += n;
                step(uint64_t(n));
            }
        }
        hash_zeros(hash, uint64_t(st.st_size - at));
        step(uint64_t(st.st_size - at));
        return hash.finish();
    }
    // The container written, the source listing checked and the lock rewritten. Returns the container files'
    // digests by identity, for the stage's verification to take as read.
    DigestCache finish(ImportObservation &observation) {
        observation.begin("packing");
        for (const auto &[file, chip] : staged) {
            put_file(file, chip, [&](uint64_t) { observation.checkpoint(); });
            fs::remove(file);
        }
        writer->finish();
        writer.reset();
        {
            const std::string text = layout.geometry(geometry);
            Fd out(lt::os::open((nand / "geometry.json").c_str(), O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC | O_NOFOLLOW, 0600));
            io(out.n >= 0, "write NAND geometry");
            write_all(out.n, text.data(), text.size());
            io(lt::os::fsync(out.n) == 0, "sync NAND geometry");
        }
        auto lock = parse(read_file(stage / "device.lock.json"));
        const Json &source = lock.at("outputs").at("nand");
        require(source.at("path").string() == "nand", "invalid NAND output path");
        Sha256 listing;
        for (const auto &d : digests) {
            const std::string line = d.first + " " + d.second + "\n";
            listing.update(line.data(), line.size());
        }
        require(listing.finish() == source.at("listing_sha256").string(), "NAND listing checksum mismatch");
        if (const Json *pages = source.find("pages"))
            require(pages->integer() == digests.size(), "NAND page count mismatch");
        DigestCache known;
        Json files;
        files.kind = Json::Object;
        Sha256 container;
        for (const char *name : {"geometry.json", nand_disk::file_name}) {
            const fs::path path = nand / name;
            auto in = open_read(path);
            lt::os::Stat st {};
            io(lt::os::fstat(in.n, &st) == 0, "stat NAND container");
            observation.begin("verifying", uint64_t(st.st_size));
            AdmissionProgress hashing{ImportObservation::notify, &observation};
            hashing.total = uint64_t(st.st_size);
            const std::string sha = digest_fd(in.n, &hashing, uint64_t(st.st_size));
            known[file_identity(path, st)] = sha;
            files.object.emplace(name, text_json(sha));
            const std::string line = std::string(name) + " " + sha + "\n";
            container.update(line.data(), line.size());
        }
        Json record;
        record.kind = Json::Object;
        record.object.emplace("path", text_json("nand"));
        record.object.emplace("listing_sha256", text_json(container.finish()));
        record.object.emplace("files", files);
        record.object.emplace("source_listing_sha256", text_json(source.at("listing_sha256").string()));
        io(lt::os::rename((stage / "device.lock.json").c_str(), (stage / "source-device.lock.json").c_str()) == 0,
           "keep source device lock");
        lock.object.at("outputs").object.at("nand") = record;
        atomic_write(stage / "device.lock.json", dump(lock) + "\n");
        return known;
    }
    static Json text_json(const std::string &value) {
        Json j;
        j.kind = Json::String;
        j.text = value;
        return j;
    }
};
// The n72 page directory (nand/cs<N>/<page>.page) or chip-file store (geometry.json and its chip files) a source
// folder's nand/ holds, by its file names; an empty layout for anything else (a container is copied as it is).
static bool n72_page_name(const std::string &relative, uint32_t &cs, uint32_t &page) {
    uint32_t slot = 0;
    if (!packed_page("nand/" + relative, slot))
        return false;
    cs = slot / packed_pages_per_cs;
    page = slot % packed_pages_per_cs;
    return true;
}
std::string prepared_guest_package(lt_device *d) {
    const auto *outputs = d->lock.find("outputs");
    const auto *package = outputs ? outputs->find("guest_package") : nullptr;
    if (!package)
        return "";
    std::string name = package->at("path").string();
    safe_name(name);
    auto path = d->base / name;
    require(digest_file(path) == package->at("sha256").string(), "prepared guest package checksum mismatch");
    return path.string();
}
void clean_device(lt_device *d, bool confirmed) {
    d->dirty = !confirmed;
    if (confirmed) {
        std::error_code ec;
        fs::remove(d->state / ".dirty", ec);
        if (ec)
            throw Failure(LT_IO, "clear dirty: " + ec.message());
        sync_dir(d->state);
    }
}
} // namespace lt
namespace lt {
std::unique_ptr<lt_device> open_device_checked(const lt_device_config *c, lt_progress_fn observer, void *opaque,
                                               const std::vector<AdmissionAssetCheck> &extra_checks) {
    lt::abi(c);
    lt::require(c->base_path && c->state_path, "invalid device configuration");
    lt::require(lt::fs::path(c->base_path).is_absolute() && lt::fs::path(c->state_path).is_absolute(),
                "device paths must be absolute");
    auto d = std::make_unique<lt_device>();
    d->base = lt::fs::canonical(c->base_path);
    lt::fs::create_directories(c->state_path);
    d->state = lt::fs::canonical(c->state_path);
    lt::require(!lt::contained(d->base, d->state) && !lt::contained(d->state, d->base), "base/state overlap refused");
    d->lease = lt::os::open((d->state / ".lease").c_str(), O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    lt::io(d->lease >= 0, "open device lease");
    lt::require(lt::os::lock_exclusive(d->lease) == 0, "device has another writer", LT_BUSY);
    AdmissionProgress events{observer, opaque};
    events.checkpoint();
    // Hashing a whole base (an iPod touch 4G NAND is 17 GB of mostly sparse pages) took over a minute
    // on a phone at every Start. Each device remembers what it verified and reads only changed files.
    auto digest_cache_path = d->state / ".verified-digests";
    const bool pinned = lt::fs::exists(d->state / ".base-identity");
    lt::DigestCache cache = pinned ? lt::read_digest_cache(digest_cache_path) : lt::DigestCache{};
    const auto cached = cache;
    lt::validate_base(d->base, &events, extra_checks, &cache);
    events.checkpoint();
    // A base is the lock's to vouch for (every file it names is hashed above), not its permissions'.
    for (const auto &entry : lt::fs::recursive_directory_iterator(d->base)) {
        events.checkpoint();
        lt::require(!entry.is_symlink(), "base symlink refused");
    }
    events.checkpoint();
    d->lock = lt::parse(lt::read_file(d->base / "device.lock.json"));
    lt::require(d->lock.at("format").integer() == 1, "unsupported lock schema", LT_UNSUPPORTED);
    d->base_digest = lt::digest_file(d->base / "device.lock.json");
    events.checkpoint();
    auto pin = d->state / ".base-identity";
    if (lt::fs::exists(pin))
        lt::require(lt::read_file(pin, 128) == d->base_digest, "state belongs to another prepared base");
    else {
        for (const auto &entry : lt::fs::directory_iterator(d->state))
            lt::require(entry.path().filename() == ".lease", "unidentified nonempty device state");
        lt::atomic_write(pin, d->base_digest);
    }
    if (cache != cached)
        lt::atomic_write(digest_cache_path, lt::dump_digest_cache(cache));
    events.checkpoint();
    d->dirty = lt::fs::exists(d->state / ".dirty");
    lt::require(!d->dirty || c->allow_dirty, "interrupted session: reopen with explicit cold recovery; data retained",
                LT_DIRTY);
    events.checkpoint();
    lt::fs::create_directories(d->state / "nand");
    if (lt::fs::exists(d->base / "nor.bin") && !lt::fs::exists(d->state / "nor.bin")) {
        auto staged = d->state / ".nor-import";
        std::error_code ec;
        lt::fs::remove(staged, ec);
        lt::copy_contents(d->base / "nor.bin", staged);
        lt::io(lt::os::rename(staged.c_str(), (d->state / "nor.bin").c_str()) == 0, "publish writable NOR");
        lt::sync_dir(d->state);
    }
    events.checkpoint();
    if (lt::fs::exists(d->state / "nor.bin"))
        lt::require(lt::fs::is_regular_file(lt::fs::symlink_status(d->state / "nor.bin")) &&
                        lt::fs::file_size(d->state / "nor.bin") == lt::fs::file_size(d->base / "nor.bin"),
                    "writable NOR invalid or truncated");
    events.checkpoint();
    return d;
}
} // namespace lt
extern "C" {
uint32_t lt_abi_version(void) { return LT_ABI_VERSION; }
lt_status lt_sha256_file(const char *p, char h[65], lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(p && h, "null path/output");
        std::string s = lt::digest_file(p);
        memcpy(h, s.c_str(), 65);
    });
}
lt_status lt_import_archive(const lt_import_config *c, lt_error *e) {
    return lt_import_archive_observed(c, nullptr, nullptr, e);
}
lt_status lt_finalize_import(const lt_finalize_import_config *c, lt_error *e) {
    return lt_finalize_import_observed(c, nullptr, nullptr, e);
}
lt_status lt_finalize_import_observed(const lt_finalize_import_config *c, lt_import_phase_fn observer,
                                     void *opaque, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(c);
        lt::ImportObservation observation{observer, opaque};
        observation.begin("verifying");
        lt::Stage stage(c->stage, c->destination);
        stage.finish(&observation);
    });
}
lt_status lt_import_archive_observed(const lt_import_config *c, lt_import_phase_fn observer, void *opaque, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(c);
        lt::require(c->source && c->expected_sha256 && strlen(c->expected_sha256) == 64,
                    "archive requires expected SHA256");
        auto input = lt::open_read(c->source);
        lt::os::Stat source_stat {};
        lt::io(lt::os::fstat(input.n, &source_stat) == 0 && source_stat.st_size >= 0, "stat archive");
        lt::ImportObservation observation{observer, opaque};
        observation.begin("hashing", uint64_t(source_stat.st_size));
        lt::AdmissionProgress hashing{lt::ImportObservation::notify, &observation};
        hashing.total = uint64_t(source_stat.st_size);
        lt::require(lt::digest_fd(input.n, &hashing, hashing.total) == c->expected_sha256, "archive SHA256 mismatch");
        lt::io(lt::os::lseek(input.n, 0, SEEK_SET) == 0, "rewind archive");
        uint8_t h[12];
        lt::read_all(input.n, h, sizeof h);
        lt::require(memcmp(h, "ITPACK01", 8) == 0, "not an ITPACK01 prepared base");
        uint32_t size = uint32_t(h[8]) | (uint32_t(h[9]) << 8) | (uint32_t(h[10]) << 16) | (uint32_t(h[11]) << 24);
        lt::require(size > 0 && size <= 64u << 20, "invalid index bound");
        std::string index(size, '\0');
        lt::read_all(input.n, index.data(), index.size());
        lt::Json j = lt::parse(index);
        const auto &entries = j.at("entries");
        lt::require(entries.kind == lt::Json::Array && !entries.array.empty() && entries.array.size() <= 1000000,
                    "invalid entry count");
        uint64_t total = 0;
        std::set<std::string> names;
        for (const auto &entry : entries.array) {
            std::string name = entry.at("name").string();
            lt::safe_name(name);
            lt::require(names.insert(name).second, "duplicate archive entry");
            uint64_t n = entry.at("size").integer();
            lt::require(n <= lt::limit(c) && total <= lt::limit(c) - n, "archive exceeds resource limit");
            total += n;
            lt::require(entry.at("mode").integer() <= 0777, "invalid entry mode");
        }
        lt::require(names.count("device.lock.json"), "missing device lock");
        for (const auto &name : names) {
            for (size_t p = name.find('/'); p != std::string::npos; p = name.find('/', p + 1))
                lt::require(!names.count(name.substr(0, p)), "file/directory conflict");
        }
        // A NAND of n72 page files (nand/csN/N.page, full 4160-byte records) or of chip files (nand/geometry.json and
        // the chips it names) becomes the record's container as it streams (ContainerImport); anything else in nand/
        // (a container already) is extracted as it is. Chip files only stream into the container once geometry.json
        // has come (the archive writer sends it first); one that comes before it is extracted and packed at the end.
        enum class Nand { Extract, Pages, Chips } route = Nand::Extract;
        {
            bool pages = false, chips = false, geometry = false, others = false;
            for (const auto &entry : entries.array) {
                const std::string name = entry.at("name").string();
                uint32_t slot = 0;
                if (name.rfind("nand/", 0) != 0)
                    continue;
                if (lt::packed_page(name, slot) && entry.at("size").integer() == lt::packed_record)
                    pages = true;
                else if (name == "nand/geometry.json")
                    geometry = true;
                else if (name.find('/', 5) == std::string::npos && name.size() > 11 &&
                         name.compare(name.size() - 6, 6, ".pages") == 0)
                    chips = true;  // checked against geometry.json as it streams
                else
                    others = true;
            }
            if (pages && !geometry && !chips && !others)
                route = Nand::Pages;
            else if (geometry && chips && !pages && !others)
                route = Nand::Chips;
        }
        lt::Stage stage(c->destination, true);
        lt::ContainerImport container(stage.path);
        if (route == Nand::Pages)
            container.begin(lt::nand_disk::n72_geometry());
        observation.begin("extracting", total);
        z_stream z{};
        lt::require(inflateInit(&z) == Z_OK, "inflate initialization failed", LT_IO);
        struct Z {
            z_stream *z;
            ~Z() { inflateEnd(z); }
        } cleanup{&z};
        std::array<uint8_t, 65536> in{}, out{};
        int status = Z_OK;
        uint64_t written = 0;
        size_t next = 0;
        uint64_t remaining = 0, target_size = 0, chip_at = 0;
        std::unique_ptr<lt::Fd> target;
        std::string nand_name;   // the NAND entry streaming now, as its listing names it
        long chip = -1;          // its chip, streaming into the container
        bool geometry_entry = false;
        std::array<uint8_t, lt::packed_record> page{};
        lt::Sha256 nand_hash;
        auto finish_target = [&] {
            if (!nand_name.empty()) {
                container.digests.emplace(nand_name, nand_hash.finish());
                if (route == Nand::Pages) {
                    uint32_t cs = 0, number = 0;
                    lt::n72_page_name(nand_name, cs, number);
                    container.put_n72(cs, number, page.data());
                }
                nand_name.clear();
                chip = -1;
            }
            if (target) {
                lt::io(lt::os::ftruncate(target->n, int64_t(target_size)) == 0, "size sparse archive entry");
                if (!stage.bulk_sync)
                    lt::io(lt::os::fsync(target->n) == 0, "sync extracted file");
                target.reset();
                // A geometry.json that names no store (no known chips) leaves the NAND as it came.
                if (geometry_entry && !lt::nand_disk::Layout::known(lt::read_file(stage.path / "nand/geometry.json", 1 << 16)))
                    route = Nand::Extract;
                else if (geometry_entry)
                    container.begin(lt::read_file(stage.path / "nand/geometry.json", 1 << 16));
                geometry_entry = false;
            }
        };
        auto advance = [&] {
            while (remaining == 0 && next < entries.array.size()) {
                observation.checkpoint();
                finish_target();
                const auto &entry = entries.array[next++];
                const std::string name = entry.at("name").string();
                remaining = target_size = entry.at("size").integer();
                chip_at = 0;
                if (route != Nand::Extract && name.rfind("nand/", 0) == 0) {
                    nand_name = name.substr(5);
                    nand_hash = lt::Sha256();
                    if (route == Nand::Pages)
                        continue;
                    geometry_entry = nand_name == "geometry.json";
                    if (!geometry_entry && container.begun()) {
                        chip = lt::ContainerImport::chip_of(container.layout, nand_name);
                        lt::require(chip >= 0 && target_size == container.layout.chip_bytes(),
                                    "NAND file " + nand_name + " is not one of geometry.json's chips");
                        continue;
                    }
                    // geometry.json, or a chip ahead of it: on disk, the chip to be packed at the end
                }
                auto path = stage.path / name;
                lt::fs::create_directories(path.parent_path());
                target = std::make_unique<lt::Fd>(
                    lt::os::open(path.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600));
                lt::io(target->n >= 0, "create archive entry");
                if (route == Nand::Chips && !nand_name.empty() && !geometry_entry)
                    container.staged.emplace_back(path, ~size_t(0));
            }
            return remaining > 0;
        };
        while (status != Z_STREAM_END) {
            if (z.avail_in == 0) {
                ssize_t n = lt::os::read(input.n, in.data(), in.size());
                lt::io(n > 0, "truncated compressed stream");
                z.next_in = in.data();
                z.avail_in = uInt(n);
            }
            z.next_out = out.data();
            z.avail_out = uInt(out.size());
            uInt before = z.avail_in;
            status = inflate(&z, Z_NO_FLUSH);
            lt::require(status == Z_OK || status == Z_STREAM_END, "corrupt zlib stream");
            size_t count = out.size() - z.avail_out, offset = 0;
            lt::require(count || before != z.avail_in || status == Z_STREAM_END, "stalled zlib stream");
            while (offset < count) {
                lt::require(advance(), "payload exceeds index");
                size_t take = size_t(std::min<uint64_t>(remaining, count - offset));
                const uint8_t *bytes = out.data() + offset;
                if (!nand_name.empty())
                    nand_hash.update(bytes, take);
                if (route == Nand::Pages && !nand_name.empty())
                    memcpy(page.data() + (lt::packed_record - remaining), bytes, take);
                else if (chip >= 0)
                    container.put(size_t(chip), chip_at, bytes, take);
                else
                    lt::sparse_write(target->n, bytes, take);
                chip_at += take;
                remaining -= take;
                offset += take;
                written += take;
            }
            lt::progress(c, written, total);
            observation.done = written;
            observation.checkpoint();
        }
        lt::require(z.avail_in == 0, "trailing archive payload");
        char extra;
        lt::require(lt::os::read(input.n, &extra, 1) == 0, "trailing archive bytes");
        lt::require(!advance() && written == total, "payload shorter than index");
        finish_target();
        lt::progress(c, total, total);
        if (route != Nand::Extract) {
            lt::require(container.begun(), "NAND chips without geometry.json");
            for (auto &[file, index] : container.staged) {
                const long k = lt::ContainerImport::chip_of(container.layout, file.filename().string());
                lt::require(k >= 0, "NAND file " + file.filename().string() + " is not one of geometry.json's chips");
                index = size_t(k);
            }
            if (route == Nand::Pages)
                lt::require(lt::parse(lt::read_file(stage.path / "device.lock.json")).at("board").string() == "n72ap",
                            "n72 NAND pages on a board other than n72");
            stage.known = container.finish(observation);
        }
        stage.finish(&observation);
    });
}
namespace lt {
// Light Touch for Mac's packed base (FirmwareKit PreparedBase, format 2), a directory: index.json names every file
// (whole, under files/) and every NAND image as 1 MiB chunks, each zlib-compressed on its own under the image's
// name less ".pages" (nand/bus0-ce0.pages -> nand/bus0-ce0/000123.z); an all-zero chunk has no file and stays a
// hole here. Each file and chunk is checked against the index's SHA-256 of its raw bytes.
static bool packed_base(const fs::path &source) {
    return fs::is_regular_file(fs::symlink_status(source / "index.json")) && fs::is_directory(source / "files");
}
static void import_packed_base(const fs::path &source, Stage &staged, const lt_import_config *c,
                               ImportObservation &observation) {
    const fs::path &stage = staged.path;
    constexpr uint64_t chunk = 1u << 20;
    Json index = parse(read_file(source / "index.json"));
    require(index.at("format").integer() == 2 && uint64_t(index.at("chunkBytes").integer()) == chunk,
            "unsupported packed base format");
    const auto &files = index.at("files"), &images = index.at("images");
    require(files.kind == Json::Array && images.kind == Json::Array, "invalid packed base index");
    // Progress counts the bytes written: whole files and stored chunks, not an image's zero holes.
    uint64_t total = 0, written = 0;
    for (const auto &entry : files.array) {
        safe_name(entry.at("name").string());
        uint64_t n = entry.at("size").integer();
        require(n <= limit(c) && total <= limit(c) - n, "packed base exceeds limit");
        total += n;
    }
    struct Chunk {
        size_t image;
        std::string number;
        std::string sha256;
        uint64_t at, length;
    };
    // Chip images become the record's NAND container (ContainerImport) when nand/ is exactly geometry.json and the
    // chips it names; anything else is unpacked as it is.
    std::unique_ptr<ContainerImport> container;
    {
        bool geometry = false, other = false;
        for (const auto &entry : files.array) {
            const std::string name = entry.at("name").string();
            geometry |= name == "nand/geometry.json";
            other |= name.rfind("nand/", 0) == 0 && name != "nand/geometry.json";
        }
        if (geometry && !other && nand_disk::Layout::known(read_file(source / "files/nand/geometry.json", 1 << 16))) {
            const auto layout = nand_disk::Layout::of(parse(read_file(source / "files/nand/geometry.json", 1 << 16)));
            bool chips = images.array.size() == layout.names.size();
            for (const auto &image : images.array) {
                const std::string name = image.at("name").string();
                chips = chips && name.rfind("nand/", 0) == 0 && ContainerImport::chip_of(layout, name.substr(5)) >= 0 &&
                        uint64_t(image.at("size").integer()) == layout.chip_bytes();
            }
            if (chips)
                container = std::make_unique<ContainerImport>(stage);
        }
    }
    std::vector<Chunk> chunks;
    std::vector<std::unique_ptr<Fd>> outputs;
    for (const auto &image : images.array) {
        const std::string name = image.at("name").string();
        safe_name(name);
        const uint64_t size = image.at("size").integer();
        require(size <= limit(c), "packed base exceeds limit");
        require(name.size() > 6 && name.compare(name.size() - 6, 6, ".pages") == 0, "unexpected packed image name");
        const auto &list = image.at("chunks");
        require(list.kind == Json::Object, "invalid packed image chunks");
        for (const auto &item : list.object) {
            require(item.first.size() == 6 && item.first.find_first_not_of("0123456789") == std::string::npos,
                    "invalid chunk number");
            const uint64_t at = std::stoull(item.first) * chunk;
            require(at < size, "chunk beyond its image");
            const uint64_t length = std::min(chunk, size - at);
            require(total <= limit(c) - length, "packed base exceeds limit");
            total += length;
            chunks.push_back({outputs.size(), item.first, item.second.string(), at, length});
        }
        if (container) {
            outputs.emplace_back();
            continue;
        }
        auto dest = stage / name;
        fs::create_directories(dest.parent_path());
        outputs.push_back(std::make_unique<Fd>(lt::os::open(dest.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600)));
        io(outputs.back()->n >= 0, "create packed image");
        io(lt::os::ftruncate(outputs.back()->n, int64_t(size)) == 0, "size packed image");
    }
    observation.total = total;
    auto report = [&](uint64_t bytes) {
        written += bytes;
        progress(c, written, total);
        observation.done = written;
        observation.checkpoint();
    };
    for (const auto &entry : files.array) {
        const std::string name = entry.at("name").string();
        auto dest = stage / name;
        fs::create_directories(dest.parent_path());
        lt::copy_contents(source / "files" / name, dest);
        require(fs::file_size(dest) == uint64_t(entry.at("size").integer()) &&
                    digest_file(dest) == entry.at("sha256").string(),
                "packed base file checksum mismatch: " + name);
        report(fs::file_size(dest));
        if (container && name == "nand/geometry.json") {
            container->begin(read_file(dest, 1 << 16));
            container->digests.emplace("geometry.json", entry.at("sha256").string());
        }
    }
    std::mutex guard;
    std::exception_ptr failure;
    if (container) {
        // A few chip images at a time, each one's chunks in order, so its digest (for the source lock's NAND
        // listing; holes hash as the zeros they read as) is taken as it streams into the container.
        std::vector<std::vector<size_t>> order(images.array.size());
        for (size_t i = 0; i < chunks.size(); ++i)
            order[chunks[i].image].push_back(i);
        for (auto &list : order)
            std::sort(list.begin(), list.end(), [&](size_t a, size_t b) { return chunks[a].at < chunks[b].at; });
        size_t next_image = 0;
        auto pack = [&] {
            std::vector<uint8_t> raw(chunk);
            for (;;) {
                size_t image;
                {
                    std::lock_guard<std::mutex> lock(guard);
                    if (failure || next_image == order.size())
                        return;
                    image = next_image++;
                }
                try {
                    const std::string name = images.array[image].at("name").string();
                    const size_t chip = size_t(ContainerImport::chip_of(container->layout, name.substr(5)));
                    const auto folder = source / name.substr(0, name.size() - 6);
                    Sha256 digest;
                    uint64_t at = 0;
                    for (size_t index : order[image]) {
                        const auto &item = chunks[index];
                        hash_zeros(digest, item.at - at);
                        const std::string packed = read_file(folder / (item.number + ".z"), 2 * chunk);
                        uLongf length = uLongf(item.length);
                        require(uncompress(raw.data(), &length, reinterpret_cast<const Bytef *>(packed.data()),
                                           uLong(packed.size())) == Z_OK && length == item.length,
                                "corrupt packed chunk " + name + " " + item.number);
                        Sha256 hash;
                        hash.update(raw.data(), length);
                        require(hash.finish() == item.sha256, "packed chunk checksum mismatch: " + name + " " + item.number);
                        digest.update(raw.data(), length);
                        container->put(chip, item.at, raw.data(), length);
                        at = item.at + length;
                        std::lock_guard<std::mutex> lock(guard);
                        if (failure)
                            return;
                        report(length);
                    }
                    hash_zeros(digest, container->layout.chip_bytes() - at);
                    std::lock_guard<std::mutex> lock(guard);
                    container->digests.emplace(name.substr(5), digest.finish());
                } catch (...) {
                    std::lock_guard<std::mutex> lock(guard);
                    if (!failure)
                        failure = std::current_exception();
                    return;
                }
            }
        };
        const size_t threads = std::min<size_t>({order.size(), 4, std::max(1u, std::thread::hardware_concurrency())});
        std::vector<std::thread> pool;
        for (size_t i = 1; i < threads; i++)
            pool.emplace_back(pack);
        pack();
        for (auto &thread : pool)
            thread.join();
        if (failure)
            std::rethrow_exception(failure);
        staged.known = container->finish(observation);
        return;
    }
    // Each chunk is read, inflated, checked and written at its own offset, a few at a time.
    size_t next = 0;
    auto unpack = [&] {
        std::vector<uint8_t> raw(chunk);
        for (;;) {
            size_t index;
            {
                std::lock_guard<std::mutex> lock(guard);
                if (failure || next == chunks.size())
                    return;
                index = next++;
            }
            try {
                const auto &item = chunks[index];
                const std::string name = images.array[item.image].at("name").string();
                const auto folder = source / name.substr(0, name.size() - 6);
                const std::string packed = read_file(folder / (item.number + ".z"), 2 * chunk);
                uLongf length = uLongf(item.length);
                require(uncompress(raw.data(), &length, reinterpret_cast<const Bytef *>(packed.data()), uLong(packed.size())) ==
                                Z_OK && length == item.length,
                        "corrupt packed chunk " + name + " " + item.number);
                Sha256 hash;
                hash.update(raw.data(), length);
                require(hash.finish() == item.sha256, "packed chunk checksum mismatch: " + name + " " + item.number);
                const int out = outputs[item.image]->n;
                for (size_t done = 0; done < length;) {
                    ssize_t n = lt::os::pwrite(out, raw.data() + done, length - done, int64_t(item.at + done));
                    if (n < 0 && errno == EINTR)
                        continue;
                    io(n > 0, "write packed chunk");
                    done += size_t(n);
                }
                std::lock_guard<std::mutex> lock(guard);
                if (!failure)
                    report(length);
            } catch (...) {
                std::lock_guard<std::mutex> lock(guard);
                if (!failure)
                    failure = std::current_exception();
                return;
            }
        }
    };
    const size_t threads = std::min<size_t>({chunks.size(), 4, std::max(1u, std::thread::hardware_concurrency())});
    std::vector<std::thread> pool;
    for (size_t i = 1; i < threads; i++)
        pool.emplace_back(unpack);
    unpack();
    for (auto &thread : pool)
        thread.join();
    if (failure)
        std::rethrow_exception(failure);
    if (!staged.bulk_sync)
        for (const auto &out : outputs)
            io(lt::os::fsync(out->n) == 0, "sync packed image");
}
} // namespace lt
lt_status lt_import_directory(const lt_import_config *c, lt_error *e) {
    return lt_import_directory_observed(c, nullptr, nullptr, e);
}
lt_status lt_import_directory_observed(const lt_import_config *c, lt_import_phase_fn observer, void *opaque, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(c);
        lt::require(c->source && lt::fs::path(c->source).is_absolute(), "source must be absolute");
        lt::require(!lt::fs::is_symlink(lt::fs::symlink_status(c->source)), "source symlink refused");
        auto source = lt::fs::canonical(c->source);
        lt::Stage stage(c->destination);
        lt::ImportObservation observation{observer, opaque};
        observation.begin("extracting");
        lt::require(!lt::contained(source, stage.path), "import destination inside source");
        if (lt::packed_base(source)) {
            lt::import_packed_base(source, stage, c, observation);
            stage.finish(&observation);
            return;
        }
        uint64_t total = 0, written = 0;
        std::vector<lt::fs::path> files, nand;
        for (const auto &entry : lt::fs::recursive_directory_iterator(source)) {
            observation.checkpoint();
            lt::require(!entry.is_symlink(), "symlink source refused");
            if (entry.is_directory())
                continue;
            lt::require(entry.is_regular_file(), "special source file refused");
            uint64_t n = entry.file_size();
            lt::require(n <= lt::limit(c) && total <= lt::limit(c) - n, "directory exceeds limit");
            total += n;
            (entry.path().lexically_relative(source).begin()->string() == "nand" ? nand : files).push_back(entry.path());
        }
        // The NAND becomes the record's container (ContainerImport) when nand/ is a store of chip files (geometry.json
        // and the chips it names) or the 2G's page files; anything else (a container already) is copied.
        lt::ContainerImport container(stage.path);
        enum class Nand { Copy, Pages, Chips } route = Nand::Copy;
        {
            std::set<std::string> names;
            for (const auto &file : nand)
                names.insert(file.lexically_relative(source / "nand").generic_string());
            uint32_t cs = 0, page = 0;
            if (names.count("geometry.json") &&
                lt::nand_disk::Layout::known(lt::read_file(source / "nand/geometry.json", 1 << 16))) {
                const auto layout = lt::nand_disk::Layout::of(lt::parse(lt::read_file(source / "nand/geometry.json", 1 << 16)));
                std::set<std::string> chips(layout.names.begin(), layout.names.end());
                chips.insert("geometry.json");
                if (chips == names)
                    route = Nand::Chips;
            } else if (!names.empty() && std::all_of(nand.begin(), nand.end(), [&](const lt::fs::path &file) {
                           return lt::n72_page_name(file.lexically_relative(source / "nand").generic_string(), cs, page) &&
                                  lt::fs::file_size(file) == lt::nand_disk::n72_record;
                       }))
                route = Nand::Pages;
        }
        if (route == Nand::Copy)
            files.insert(files.end(), nand.begin(), nand.end());
        observation.total = total;
        observation.checkpoint();
        auto step = [&](uint64_t bytes) {
            written += bytes;
            lt::progress(c, written, total);
            observation.done = written;
            observation.checkpoint();
        };
        for (const auto &file : files) {
            auto relative = file.lexically_relative(source);
            lt::safe_name(relative.generic_string());
            auto dest = stage.path / relative;
            lt::fs::create_directories(dest.parent_path());
            lt::copy_contents(file, dest);
            step(lt::fs::file_size(file));
        }
        if (route == Nand::Chips) {
            container.begin(lt::read_file(source / "nand/geometry.json", 1 << 16));
            container.digests.emplace("geometry.json", lt::digest_file(source / "nand/geometry.json"));
            for (size_t chip = 0; chip < container.layout.names.size(); ++chip) {
                const auto &name = container.layout.names[chip];
                container.digests.emplace(name, container.put_file(source / "nand" / name, chip, step));
            }
        } else if (route == Nand::Pages) {
            container.begin(lt::nand_disk::n72_geometry());
            std::array<uint8_t, lt::nand_disk::n72_record> record{};
            for (const auto &file : nand) {
                const std::string name = file.lexically_relative(source / "nand").generic_string();
                uint32_t cs = 0, page = 0;
                lt::n72_page_name(name, cs, page);
                auto in = lt::open_read(file);
                lt::read_all(in.n, record.data(), record.size());
                lt::Sha256 hash;
                hash.update(record.data(), record.size());
                container.digests.emplace(name, hash.finish());
                container.put_n72(cs, page, record.data());
                step(record.size());
            }
        }
        if (route != Nand::Copy)
            stage.known = container.finish(observation);
        stage.finish(&observation);
    });
}
lt_status lt_device_open(const lt_device_config *c, lt_device **out, lt_error *e) {
    return lt_device_open_observed(c, nullptr, nullptr, out, e);
}
lt_status lt_device_open_observed(const lt_device_config *c, lt_progress_fn observer, void *opaque, lt_device **out,
                                  lt_error *e) {
    if (out)
        *out = nullptr;
    return lt::guard(e, [&] {
        lt::require(out, "device output missing");
        *out = lt::open_device_checked(c, observer, opaque, {}).release();
    });
}
lt_status lt_device_describe(lt_device *d, lt_device_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(d, "null device");
        lt::abi(out);
        snprintf(out->board, sizeof out->board, "%s", d->lock.at("board").string().c_str());
        snprintf(out->build, sizeof out->build, "%s", d->lock.at("build").string().c_str());
        const auto *s = d->lock.find("boot_strategy");
        snprintf(out->boot_strategy, sizeof out->boot_strategy, "%s", s ? s->string().c_str() : "");
        snprintf(out->base_sha256, sizeof out->base_sha256, "%s", d->base_digest.c_str());
        out->dirty = d->dirty;
    });
}
void lt_device_close(lt_device *d) {
    if (d && !d->active)
        delete d;
}
namespace lt {
// FirmwareKit's image stores of chip files (geometry.json storage_format nand-xor-blank-v2: the iPod touch 2G's, as
// earlier preparations made them) are written in place, with no page overlay: a device boots a copy of its base's
// store in state/nand, made on its first boot (and again after an erase removes it), as upstream's HostRuntime
// clones it. Only the chips' data extents are copied; the rest stays holes, which read as blank pages. A NAND
// container (geometry.json "container": "qcow2", every board's since) is never copied: the engine writes the
// device's own overlay, state/nand/nand.qcow2, over the base's nand.qcow2.
static bool image_store_in_place(const fs::path &nand) {
    if (!fs::is_regular_file(nand / "geometry.json"))
        return false;
    auto geometry = parse(read_file(nand / "geometry.json", 1 << 16));
    const auto *format = geometry.find("storage_format");
    return format && format->kind == Json::String && format->string() == "nand-xor-blank-v2" &&
           !geometry.find("container");
}
static fs::path live_image_store(const fs::path &base, const fs::path &state) {
    const fs::path live = state / "nand";
    if (fs::is_regular_file(live / "geometry.json"))
        return live;
    require(!fs::exists(live) || fs::is_empty(live), "this device's data was written in an older format over a newer system");
    const fs::path staged = state / ".nand-clone.tmp";
    fs::remove_all(staged);
    fs::create_directories(staged);
    for (const auto &entry : fs::directory_iterator(base))
        clone_sparse(entry.path(), staged / entry.path().filename());
    fs::remove_all(live);
    fs::rename(staged, live);
    return live;
}
}  // namespace lt
lt_status lt_device_boot_argv(lt_device *d, const lt_boot_config *c, int *argc, const char *const **argv, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(d && argc && argv, "null boot output");
        lt::abi(c);
        lt::require(!d->active, "device running", LT_BUSY);
        lt::require(c->machine && *c->machine, "engine machine facts required");
        const std::string board = d->lock.at("board").string();
        const auto route = lt::boot_route(board, d->lock);
        const bool n72 = route.family == lt::BootFamily::S5L8720, s5l8900 = route.family == lt::BootFamily::S5L8900;
        if (route.needs_bootrom())
            lt::require(c->bootrom_path && lt::fs::is_regular_file(c->bootrom_path),
                        "this device needs its boot ROM (" + std::string(s5l8900 ? "S5L8900" : "iBoot-240.4") + ")");
        lt::require(lt::fs::exists(d->base / "nand"), "base missing nand");
        for (const auto &output : route.outputs)
            lt::require(lt::fs::is_regular_file(d->base / output.second), "base missing " + output.second);
        const bool nor = lt::fs::is_regular_file(d->base / "nor.bin");
        auto escape = [](const std::string &s) {
            std::string o;
            for (char b : s) {
                lt::require(b != '\0', "embedded null");
                o += b;
                if (b == ',')
                    o += ',';
            }
            return o;
        };
        // An image store of chip files (the 2G's and the 1.x boards'): the emulator writes a live copy in place,
        // with no overlay. Everything else, containers included, takes the device's state/nand as its overlay.
        const bool image_store = route.family != lt::BootFamily::A4 && lt::image_store_in_place(d->base / "nand");
        std::string machine = c->machine;
        auto option = [&](const std::string &k, const std::string &v) { machine += "," + k + "=" + escape(v); };
        if (n72) {
            option("boot-args", "amfi_allow_any_signature=1 cs_enforcement_disable=1");
            option("direct-iboot", route.strategy == "iboot" ? (d->base / "iBoot.bin").string() : "");
            option("direct-llb", "");
            option("bootrom", c->bootrom_path);
            option("nor", (d->base / "nor.bin").string());
            if (!image_store)
                option("nandrw", (d->state / "nand").string());
            option("gid-blobs", (d->base / "gid-blobs.bin").string());
            option("wifi", "on");
        } else if (s5l8900) {
            option("bootrom", c->bootrom_path);
            option("iboot", (d->base / "iBoot.bin").string());
            if (!image_store)
                option("nand-overlay", (d->state / "nand").string());
        } else {
            if (route.strategy == "kboot")
                option("kboot", (d->base / "kboot.bin").string());
            else {
                option("iboot", (d->base / "iBoot.bin").string());
                option("gid-blobs", (d->base / "gid-blobs.bin").string());
            }
            option("nand-overlay", (d->state / "nand").string());
            if (lt::fs::exists(d->base / "identity.json")) {
                auto identity = lt::parse(lt::read_file(d->base / "identity.json"));
                if (const auto *die = identity.find("die-id")) {
                    lt::require(die->kind == lt::Json::Array, "invalid die-id");
                    std::string value;
                    for (const auto &word : die->array) {
                        if (!value.empty())
                            value += ":";
                        value += word.string();
                    }
                    option("die-id", value);
                }
            }
            if (route.modem)
                option("baseband", "on");
        }
        option("nand", image_store ? lt::live_image_store(d->base / "nand", d->state).string() : (d->base / "nand").string());
        // The S5L8900's NOR is a pflash drive (below); the others' a machine property.
        if (nor && !s5l8900)
            option("nor-rw", (d->state / "nor.bin").string());
        std::string package =
            c->guest_package_path && *c->guest_package_path ? c->guest_package_path : lt::prepared_guest_package(d);
        if (!package.empty())
            option("guest-package", lt::guest_offer(d, package.c_str()));
        if (const auto *m = d->lock.find("machine")) {
            lt::require(m->kind == lt::Json::Object, "invalid machine metadata");
            for (const auto &v : m->object) {
                lt::require(v.first.find_first_not_of("abcdefghijklmnopqrstuvwxyz0123456789-") == std::string::npos,
                            "invalid machine option key");
                lt::require(v.second.kind == lt::Json::String || v.second.kind == lt::Json::Number ||
                                v.second.kind == lt::Json::Boolean,
                            "invalid machine option value");
                static const std::set<std::string> safe_options = {"wifi-mac", "bt-mac", "ecid", "aes-uid",
                                                                   "rtc-epoch", "imei"};
                lt::require(safe_options.count(v.first), "unsupported prepared machine property: " + v.first,
                            LT_UNSUPPORTED);
                // FirmwareKit leaves a property the unit has no value for empty: the machine's default then.
                if (v.second.kind == lt::Json::String && v.second.string().empty())
                    continue;
                option(v.first,
                       v.second.kind == lt::Json::Boolean ? (v.second.text == "true" ? "on" : "off") : v.second.text);
            }
        }
        // iOS then routes sound to the jack: stereo, without its speaker EQ and limiter (which cost guest CPU).
        if (c->headphones)
            option("headphones", "on");
        d->args = {"LightTouch", "-M", machine};
        lt::require(c->network_mode <= 2, "Invalid network mode");
        std::string network = "user,id=wifi0,lan=" + std::string(c->network_local && c->network_mode == 1 ? "on" : "off") +
            ",restrict=" + (c->network_mode == 1 ? "off" : "on");
        // The proxy forward is there whatever the mode, so networking can be turned on while the guest runs: with no
        // proxy listening yet a guest connection to it just closes, and Off still blocks every other destination.
        if (c->network_socket) {
            // An absolute path that fits sockaddr_un everywhere (104 bytes on macOS); on Windows a drive path.
            const char *endpoint = c->network_socket;
#ifdef _WIN32
            const bool absolute = endpoint && (endpoint[0] | 32) >= 'a' && (endpoint[0] | 32) <= 'z' &&
                                  endpoint[1] == ':' && (endpoint[2] == '\\' || endpoint[2] == '/');
#else
            const bool absolute = endpoint && endpoint[0] == '/';
#endif
            lt::require(absolute && std::strlen(c->network_socket) < 104 &&
                        std::strchr(c->network_socket, '\n') == nullptr, "Invalid private network endpoint");
            network += ",guestfwd=tcp:10.0.2.100:3128-unix:" + escape(c->network_socket);
        }
        d->args.insert(d->args.end(), {"-netdev", network});
        // Chain TCG jumps across guest pages: iOS never remaps a code page to other code
        // in place (the engine's cross-page-chain property documents the assumption).
        // Interleaved A/B: Poco F3 Flight Control 19.8 -> 24.5 fps (6 pairs), Pixel 10
        // Doodle Jump menu 15.3 -> 18.0. LT_CROSS_PAGE_CHAIN=off turns it off for A/B runs.
        // Indirect branches (returns, objc_msgSend) cache their last target in the TB, per
        // address space, under the same no-remap condition. LT_INDIRECT_CACHE=off for A/B.
        const char *chain = getenv("LT_CROSS_PAGE_CHAIN");
        const char *indirect = getenv("LT_INDIRECT_CACHE");
        std::string accel = "tcg";
        if (!chain || strcmp(chain, "off") != 0) accel += ",cross-page-chain=on";
        if (!indirect || strcmp(indirect, "off") != 0) accel += ",indirect-cache=on";
        if (accel != "tcg") {
            d->args.push_back("-accel");
            d->args.push_back(accel);
        }
        if (n72) {
            d->args.push_back("-m");
            d->args.push_back("128M");
#ifdef __ANDROID__
            // Timer 4 at the hardware's 6 MHz rather than the model's 10 MHz default.
            // Measured on a Pixel 10 and Poco F3 (2026-10-17, interleaved x2, frame-woken
            // presenter): delivered animation fps equal or better (Poco 44 -> 49), share of
            // frames on the 16.7 ms cadence equal or better (Poco 62% -> 75%), idle engine
            // CPU 5-13% lower. Desktop stays at 10 MHz: ipod_touch_timer.c records a
            // desktop measurement where 6 MHz made frame pacing worse.
            d->args.push_back("-global");
            d->args.push_back("driver=ipodtouch.timer,property=freq-hz,value=6000000");
#endif
        }
        if (s5l8900) {
            std::string drive = "if=pflash,format=raw,file=" + escape((d->state / "nor.bin").string());
            d->args.insert(d->args.end(), {"-drive", drive});
        }
        // The SIM keeps its PIN, PUK and tries left with the device's own state, as a card in the tray does.
        if (route.modem)
            d->args.insert(d->args.end(), {"-global", "ios-baseband.sim-file=" + (d->state / "sim").string()});
        d->args.insert(d->args.end(),
                       {"-display", "none", "-no-shutdown", "-audio", "driver=none", "-serial",
                        c->serial_path && *c->serial_path ? std::string("file:") + c->serial_path : "null"});
        d->argv.clear();
        for (const auto &s : d->args)
            d->argv.push_back(s.c_str());
        *argc = int(d->argv.size());
        *argv = d->argv.data();
    });
}
}
lt_device::~lt_device() {
    if (lease >= 0)
        lt::os::close(lease);
}
