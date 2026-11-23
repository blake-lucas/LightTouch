// Device transfer: a stopped record as one ZIP64 file, and such a file added back as a new record, on any host.
//
// The file is a standard zip (deflate, ZIP64, UTF-8 names), written and read strictly in order so a pipe works on
// either side (Android's Storage Access Framework hands out pipes for some providers). Its first entry is
// lighttouch-device.json, the manifest: format and version, the app that wrote it, the device's name, board, build and
// guest tools serial, and every file with its size, so an import refuses an unsupported or damaged file and checks
// free space before writing anything large. Each entry's CRC-32 (in its data descriptor and the central directory)
// guards its bytes; the base is also checked against its own lock's SHA-256 digests, hashed as it arrives.
//
// What goes in: record.json, carrier.json, base/ (the prepared system), assets/ (boot ROM, guest tools) and state/
// (the device's own NAND overlay, NOR, SIM and guest verdicts; the unclean-shutdown mark when there is one). What
// stays out: the lease, the admission cache (.base-identity, .verified-digests: inode identities of this host), the
// guest-offer staging (state/work), logs and temporary files. Pairing and network folders live outside the record and
// are made again on the new host. There is no saved running state to carry: a moved device cold-boots.
//
// Holes: an export never reads a file's holes (data_ranges); a chunk that is all zeros is one precompressed block.
// An import writes zeros as holes and sets each file's length, so sparse images stay sparse.
//
// Speed: a file is cut into 1 MiB chunks, each deflated on its own (level 1, sync-flushed, so the chunks concatenate
// into one deflate stream) on all cores; the CRC-32s combine. Import inflates one stream per entry.
#include "Internal.hpp"
#include "Records.hpp"
#include "Os.hpp"
#include <zlib.h>
#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <ctime>
#include <fcntl.h>
#include <map>
#include <mutex>
#include <set>
#include <thread>

namespace lt {
namespace {
constexpr const char *manifest_name = "lighttouch-device.json";
constexpr const char *format_name = "lighttouch-device";
constexpr int64_t format_version = 1;
constexpr size_t chunk_bytes = 1u << 20;
constexpr uint32_t local_signature = 0x04034b50, central_signature = 0x02014b50, descriptor_signature = 0x08074b50;
constexpr const char *damaged = "This device file is damaged or incomplete";

// The observer, rate-limited: a callback per megabyte would wake a UI hundreds of times a second.
struct Observer {
    lt_import_phase_fn fn;
    void *opaque;
    std::string phase;
    uint64_t done = 0, total = 0;
    std::chrono::steady_clock::time_point last{};
    void begin(const char *name, uint64_t bytes) {
        phase = name;
        done = 0;
        total = bytes;
        report(true);
    }
    void add(uint64_t bytes) {
        done += bytes;
        report(false);
    }
    void report(bool force) {
        const auto now = std::chrono::steady_clock::now();
        if (!force && now - last < std::chrono::milliseconds(100))
            return;
        last = now;
        if (fn && fn(opaque, phase.c_str(), done, total) != 0)
            throw Failure(LT_CANCELLED, "cancelled");
    }
};

struct OwnedFd {
    int n = -1;
    OwnedFd() = default;
    explicit OwnedFd(int fd) : n(fd) {}
    OwnedFd(const OwnedFd &) = delete;
    OwnedFd &operator=(const OwnedFd &) = delete;
    ~OwnedFd() { reset(); }
    void reset(int fd = -1) {
        if (n >= 0)
            lt::os::close(n);
        n = fd;
    }
};

void write_fully(int fd, const uint8_t *p, size_t n) {
    while (n) {
        ssize_t k = lt::os::write(fd, p, n);
        if (k < 0 && errno == EINTR)
            continue;
        if (k < 0 && errno == ENOSPC)
            throw Failure(LT_IO, "There isn't enough free space to save the device file");
        io(k > 0, "write device file");
        p += k;
        n -= size_t(k);
    }
}

bool all_zero(const uint8_t *p, size_t n) {
    size_t i = 0;
    for (; i + 8 <= n; i += 8) {
        uint64_t word;
        std::memcpy(&word, p + i, 8);
        if (word)
            return false;
    }
    for (; i < n; ++i)
        if (p[i])
            return false;
    return true;
}

// Names an export writes and an import accepts: relative, '/'-separated, no empty, "." or ".." parts, nothing hidden
// but the unclean-shutdown mark, and only under the record's own parts.
bool entry_name_ok(const std::string &name) {
    if (name.empty() || name.size() > 1024 || name.find_first_of(std::string("\\:\0", 3)) != std::string::npos)
        return false;
    if (name == "record.json" || name == "carrier.json" || name == "state/.dirty")
        return true;
    if (name.rfind("base/", 0) && name.rfind("assets/", 0) && name.rfind("state/", 0))
        return false;
    size_t start = 0;
    for (;;) {
        const size_t end = name.find('/', start);
        const std::string part = name.substr(start, end == std::string::npos ? std::string::npos : end - start);
        if (part.empty() || part[0] == '.')
            return false;
        for (unsigned char c : part)
            if (c < 0x20 || c == 0x7f)
                return false;
        if (end == std::string::npos)
            return true;
        start = end + 1;
    }
}

void put16(std::vector<uint8_t> &b, uint32_t v) {
    b.push_back(uint8_t(v));
    b.push_back(uint8_t(v >> 8));
}
void put32(std::vector<uint8_t> &b, uint32_t v) {
    put16(b, v & 0xffff);
    put16(b, v >> 16);
}
void put64(std::vector<uint8_t> &b, uint64_t v) {
    put32(b, uint32_t(v));
    put32(b, uint32_t(v >> 32));
}

// ---- export ----

struct Entry {
    std::string name;
    fs::path path;     // empty: [bytes] is the content
    std::string bytes;
    uint64_t size = 0, data = 0;
    std::vector<std::pair<int64_t, int64_t>> ranges;  // where the file has data
};

struct Job {
    size_t entry;
    uint64_t offset, length;
    bool hole;
};

struct Out {
    int fd;
    uint64_t offset = 0;
    std::vector<uint8_t> buffer;
    void put(const uint8_t *p, size_t n) {
        if (n >= (1u << 20)) {
            flush();
            write_fully(fd, p, n);
        } else {
            if (buffer.size() + n > (2u << 20))
                flush();
            buffer.insert(buffer.end(), p, p + n);
        }
        offset += n;
    }
    void put(const std::vector<uint8_t> &b) { put(b.data(), b.size()); }
    void flush() {
        if (!buffer.empty())
            write_fully(fd, buffer.data(), buffer.size());
        buffer.clear();
    }
};

struct Central {
    std::string name;
    uint32_t crc = 0;
    uint64_t compressed = 0, size = 0, offset = 0;
};

struct Deflater {
    z_stream z{};
    Deflater() { require(deflateInit2(&z, 1, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY) == Z_OK, "deflate setup", LT_IO); }
    ~Deflater() { deflateEnd(&z); }
    // One chunk as a sync-flushed raw deflate segment: byte-aligned and free of back-references to earlier chunks, so
    // segments made apart concatenate into one stream.
    std::vector<uint8_t> chunk(const uint8_t *p, size_t n) {
        require(deflateReset(&z) == Z_OK, "deflate reset", LT_IO);
        std::vector<uint8_t> out(deflateBound(&z, uLong(n)) + 64);
        z.next_in = const_cast<Bytef *>(p);
        z.avail_in = uInt(n);
        size_t produced = 0;
        for (;;) {
            z.next_out = out.data() + produced;
            z.avail_out = uInt(out.size() - produced);
            const int r = deflate(&z, Z_SYNC_FLUSH);
            require(r == Z_OK || r == Z_BUF_ERROR, "deflate", LT_IO);
            produced = out.size() - z.avail_out;
            if (z.avail_out != 0 && z.avail_in == 0)
                break;
            out.resize(out.size() * 2);
        }
        out.resize(produced);
        return out;
    }
};

void dos_time(uint16_t &time, uint16_t &date) {
    std::time_t now = std::time(nullptr);
    std::tm t{};
#ifdef _WIN32
    gmtime_s(&t, &now);
#else
    gmtime_r(&now, &t);
#endif
    time = uint16_t((t.tm_hour << 11) | (t.tm_min << 5) | (t.tm_sec / 2));
    date = uint16_t(((std::max(t.tm_year, 80) - 80) << 9) | ((t.tm_mon + 1) << 5) | t.tm_mday);
}

std::string iso_now() {
    std::time_t now = std::time(nullptr);
    std::tm t{};
#ifdef _WIN32
    gmtime_s(&t, &now);
#else
    gmtime_r(&now, &t);
#endif
    char text[32];
    std::strftime(text, sizeof text, "%Y-%m-%dT%H:%M:%SZ", &t);
    return text;
}

// The record's files, in the order the file carries them: what an import needs first (its identity and lock), then
// the small ones, then the images.
std::vector<Entry> collect(const fs::path &directory) {
    std::vector<Entry> small, rest;
    auto add = [&](const fs::path &path, const std::string &name) {
        require(entry_name_ok(name), "this device has a file a device export can't carry: " + name, LT_UNSUPPORTED);
        lt::os::Stat st{};
        io(lt::os::lstat(path.c_str(), &st) == 0, "inspect " + name);
        require(S_ISREG(st.st_mode) && st.st_size >= 0, "this device has a file that isn't a regular file: " + name,
                LT_UNSUPPORTED);
        Entry e;
        e.name = name;
        e.path = path;
        e.size = uint64_t(st.st_size);
        if (e.size) {
            OwnedFd fd(lt::os::open(path.c_str(), O_RDONLY | O_CLOEXEC));
            io(fd.n >= 0, "open " + name);
            e.ranges = file_data_ranges(fd.n, int64_t(e.size));
            for (const auto &r : e.ranges)
                e.data += uint64_t(r.second - r.first);
        }
        (e.size <= (4u << 20) ? small : rest).push_back(std::move(e));
    };
    auto tree = [&](const std::string &part, auto keep) {
        const fs::path root = directory / part;
        if (!fs::is_directory(fs::symlink_status(root)))
            return;
        std::vector<std::pair<std::string, fs::path>> files;
        for (auto it = fs::recursive_directory_iterator(root); it != fs::recursive_directory_iterator(); ++it) {
            const std::string relative = it->path().lexically_relative(root).generic_string();
            if (!keep(relative)) {
                std::error_code ec;
                if (it->is_directory(ec))
                    it.disable_recursion_pending();
                continue;
            }
            require(!it->is_symlink(), "this device has a link a device export can't carry: " + part + "/" + relative,
                    LT_UNSUPPORTED);
            if (it->is_directory())
                continue;
            files.emplace_back(part + "/" + relative, it->path());
        }
        std::sort(files.begin(), files.end());
        for (const auto &f : files)
            add(f.second, f.first);
    };
    // Hidden names anywhere are this app's temporaries (and the lease, and the admission cache).
    auto visible = [](const std::string &relative) {
        size_t start = 0;
        for (;;) {
            if (relative[start] == '.')
                return false;
            const size_t slash = relative.find('/', start);
            if (slash == std::string::npos)
                return true;
            start = slash + 1;
        }
    };
    add(directory / "record.json", "record.json");
    add(directory / "base/device.lock.json", "base/device.lock.json");
    if (fs::is_regular_file(fs::symlink_status(directory / "carrier.json")))
        add(directory / "carrier.json", "carrier.json");
    tree("assets", visible);
    tree("base", [&](const std::string &r) { return visible(r) && r != "device.lock.json"; });
    if (fs::is_regular_file(fs::symlink_status(directory / "state/.dirty")))
        add(directory / "state/.dirty", "state/.dirty");
    tree("state", [&](const std::string &r) {
        const bool top = r.find('/') == std::string::npos;
        // work/: the guest offer staged for each boot; logs: this host's.
        return visible(r) && r != "work" && r.rfind("work/", 0) && !(top && r.size() > 4 && r.compare(r.size() - 4, 4, ".log") == 0);
    });
    // Identity and lock first (in that order), the rest of the small files, then the large.
    std::vector<Entry> all;
    for (auto *list : {&small, &rest})
        for (auto &e : *list)
            all.push_back(std::move(e));
    std::stable_partition(all.begin(), all.end(), [](const Entry &e) {
        return e.name == "record.json" || e.name == "base/device.lock.json";
    });
    return all;
}

class Exporter {
public:
    Exporter(std::vector<Entry> &entries, int fd, Observer &observer) : entries(entries), out{fd, 0, {}}, observer(observer) {}

    void run() {
        dos_time(time, date);
        for (size_t i = 0; i < entries.size(); ++i) {
            const auto &e = entries[i];
            for (uint64_t at = 0; at < e.size; at += chunk_bytes) {
                const uint64_t length = std::min<uint64_t>(chunk_bytes, e.size - at);
                bool hole = !e.path.empty();
                for (const auto &r : e.ranges)
                    if (uint64_t(r.first) < at + length && uint64_t(r.second) > at) {
                        hole = false;
                        break;
                    }
                jobs.push_back({i, at, length, hole});
            }
        }
        {
            std::vector<uint8_t> zeros(chunk_bytes, 0);
            Deflater d;
            zero_chunk = d.chunk(zeros.data(), zeros.size());
            zero_crc = uint32_t(crc32(0, zeros.data(), uInt(zeros.size())));
        }
        const unsigned cores = std::max(1u, std::thread::hardware_concurrency());
        const size_t threads = std::min<size_t>(cores, 8);
        window = threads * 3;
        slots.resize(window);
        std::vector<std::thread> pool;
        for (size_t t = 0; t < threads; ++t)
            pool.emplace_back([this] { work(); });
        try {
            write_entries();
        } catch (...) {
            {
                std::lock_guard<std::mutex> lock(guard);
                stop = true;
            }
            wake.notify_all();
            for (auto &t : pool)
                t.join();
            throw;
        }
        for (auto &t : pool)
            t.join();
        finish();
    }

private:
    struct Slot {
        std::vector<uint8_t> data;
        uint32_t crc = 0;
        bool ready = false;
    };
    std::vector<Entry> &entries;
    Out out;
    Observer &observer;
    std::vector<Job> jobs;
    std::vector<Central> central;
    std::vector<uint8_t> zero_chunk;
    uint32_t zero_crc = 0;
    uint16_t time = 0, date = 0;
    size_t window = 1;
    std::vector<Slot> slots;
    std::mutex guard;
    std::condition_variable wake;
    size_t next = 0, consumed = 0;
    bool stop = false;
    std::exception_ptr failure;

    void work() {
        Deflater deflater;
        std::vector<uint8_t> buffer(chunk_bytes);
        for (;;) {
            size_t index;
            {
                std::unique_lock<std::mutex> lock(guard);
                wake.wait(lock, [&] { return stop || next >= jobs.size() || next < consumed + window; });
                if (stop || next >= jobs.size())
                    return;
                index = next++;
            }
            Slot result;
            try {
                const Job &job = jobs[index];
                const Entry &e = entries[job.entry];
                const uint8_t *data = buffer.data();
                if (job.hole) {
                    std::fill(buffer.begin(), buffer.begin() + std::ptrdiff_t(job.length), uint8_t(0));
                } else if (e.path.empty()) {
                    data = reinterpret_cast<const uint8_t *>(e.bytes.data()) + job.offset;
                } else {
                    OwnedFd fd(lt::os::open(e.path.c_str(), O_RDONLY | O_CLOEXEC));
                    io(fd.n >= 0, "open " + e.name);
                    for (uint64_t done = 0; done < job.length;) {
                        ssize_t n = lt::os::pread(fd.n, buffer.data() + done, size_t(job.length - done),
                                                  int64_t(job.offset + done));
                        if (n < 0 && errno == EINTR)
                            continue;
                        io(n >= 0, "read " + e.name);
                        require(n > 0, e.name + " changed while it was exported", LT_IO);
                        done += uint64_t(n);
                    }
                }
                if (job.length == chunk_bytes && (job.hole || all_zero(data, size_t(job.length)))) {
                    result.data = zero_chunk;
                    result.crc = zero_crc;
                } else {
                    result.data = deflater.chunk(data, size_t(job.length));
                    result.crc = uint32_t(crc32(0, data, uInt(job.length)));
                }
            } catch (...) {
                std::lock_guard<std::mutex> lock(guard);
                if (!failure)
                    failure = std::current_exception();
                stop = true;
                wake.notify_all();
                return;
            }
            {
                std::lock_guard<std::mutex> lock(guard);
                result.ready = true;
                slots[index % window] = std::move(result);
            }
            wake.notify_all();
        }
    }

    Slot take(size_t index) {
        std::unique_lock<std::mutex> lock(guard);
        wake.wait(lock, [&] { return failure || slots[index % window].ready; });
        if (failure)
            std::rethrow_exception(failure);
        Slot s = std::move(slots[index % window]);
        slots[index % window] = Slot{};
        consumed = index + 1;
        lock.unlock();
        wake.notify_all();
        return s;
    }

    void write_entries() {
        size_t job = 0;
        for (size_t i = 0; i < entries.size(); ++i) {
            const Entry &e = entries[i];
            Central c;
            c.name = e.name;
            c.size = e.size;
            c.offset = out.offset;
            std::vector<uint8_t> h;
            put32(h, local_signature);
            put16(h, 45);                 // version needed: ZIP64
            put16(h, 0x0808);             // sizes in the data descriptor; UTF-8 name
            put16(h, 8);                  // deflate
            put16(h, time);
            put16(h, date);
            put32(h, 0);                  // CRC-32: in the data descriptor
            put32(h, 0xffffffff);         // sizes: in the ZIP64 field, then the descriptor
            put32(h, 0xffffffff);
            put16(h, uint16_t(e.name.size()));
            put16(h, 20);
            h.insert(h.end(), e.name.begin(), e.name.end());
            put16(h, 1);                  // ZIP64 extended information
            put16(h, 16);
            put64(h, 0);
            put64(h, 0);
            out.put(h);
            const uint64_t start = out.offset;
            uint32_t crc = 0;
            for (; job < jobs.size() && jobs[job].entry == i; ++job) {
                Slot s = take(job);
                out.put(s.data);
                crc = uint32_t(crc32_combine(crc, s.crc, static_cast<z_off_t>(jobs[job].length)));
                observer.add(jobs[job].length);
            }
            static const uint8_t final_block[2] = {0x03, 0x00};
            out.put(final_block, 2);
            c.crc = crc;
            c.compressed = out.offset - start;
            std::vector<uint8_t> d;
            put32(d, descriptor_signature);
            put32(d, crc);
            put64(d, c.compressed);
            put64(d, c.size);
            out.put(d);
            central.push_back(std::move(c));
            observer.report(false);
        }
    }

    void finish() {
        const uint64_t directory = out.offset;
        for (const auto &c : central) {
            std::vector<uint8_t> extra;
            const bool big_size = c.size >= 0xffffffffu, big_compressed = c.compressed >= 0xffffffffu,
                       big_offset = c.offset >= 0xffffffffu;
            if (big_size)
                put64(extra, c.size);
            if (big_compressed)
                put64(extra, c.compressed);
            if (big_offset)
                put64(extra, c.offset);
            std::vector<uint8_t> h;
            put32(h, central_signature);
            put16(h, 45);
            put16(h, 45);
            put16(h, 0x0808);
            put16(h, 8);
            put16(h, time);
            put16(h, date);
            put32(h, c.crc);
            put32(h, big_compressed ? 0xffffffffu : uint32_t(c.compressed));
            put32(h, big_size ? 0xffffffffu : uint32_t(c.size));
            put16(h, uint16_t(c.name.size()));
            put16(h, uint16_t(extra.empty() ? 0 : extra.size() + 4));
            put16(h, 0);  // comment
            put16(h, 0);  // disk
            put16(h, 0);  // internal attributes
            put32(h, 0);  // external attributes
            put32(h, big_offset ? 0xffffffffu : uint32_t(c.offset));
            h.insert(h.end(), c.name.begin(), c.name.end());
            if (!extra.empty()) {
                put16(h, 1);
                put16(h, uint16_t(extra.size()));
                h.insert(h.end(), extra.begin(), extra.end());
            }
            out.put(h);
        }
        const uint64_t directory_size = out.offset - directory, end64 = out.offset;
        std::vector<uint8_t> t;
        put32(t, 0x06064b50);  // ZIP64 end of central directory
        put64(t, 44);
        put16(t, 45);
        put16(t, 45);
        put32(t, 0);
        put32(t, 0);
        put64(t, central.size());
        put64(t, central.size());
        put64(t, directory_size);
        put64(t, directory);
        put32(t, 0x07064b50);  // its locator
        put32(t, 0);
        put64(t, end64);
        put32(t, 1);
        put32(t, 0x06054b50);  // end of central directory
        put16(t, 0);
        put16(t, 0);
        put16(t, uint16_t(std::min<size_t>(central.size(), 0xffff)));
        put16(t, uint16_t(std::min<size_t>(central.size(), 0xffff)));
        put32(t, uint32_t(std::min<uint64_t>(directory_size, 0xffffffffu)));
        put32(t, uint32_t(std::min<uint64_t>(directory, 0xffffffffu)));
        put16(t, 0);
        out.put(t);
        out.flush();
    }
};

// The library lock, waited for: a list or select elsewhere holds it for moments, and a long import shouldn't fail
// at its last step for that.
std::unique_ptr<LibraryLock> wait_lock(lt_library *l) {
    for (int attempt = 0;; ++attempt) {
        try {
            return std::make_unique<LibraryLock>(l);
        } catch (const Failure &f) {
            if (f.status != LT_BUSY || attempt >= 200)
                throw;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(50));
    }
}

void export_record(lt_library *l, const lt_export_config *c, Observer &observer) {
    abi(c);
    require(c->record_id, "record ID missing");
    const std::string id = c->record_id;
    record_id(id);
    const fs::path directory = l->root / "records" / id;
    {
        auto lock = wait_lock(l);
        require(fs::is_directory(fs::symlink_status(directory)), "no such device");
    }
    auto metadata = parse(read_file(directory / "record.json", 65536));
    require(metadata.at("schema").integer() == 1 && metadata.at("id").string() == id, "invalid device record");
    const std::string name = metadata.at("name").string(), board = metadata.at("board").string(),
                      build = metadata.at("build").string();
    // The device's own lease, held to the end: it can't start, be deleted or change its guest tools meanwhile.
    fs::create_directories(directory / "state");
    OwnedFd lease(lt::os::open((directory / "state/.lease").c_str(), O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600));
    io(lease.n >= 0, "device lease");
    require(lt::os::lock_exclusive(lease.n) == 0, "Stop " + name + " before exporting it", LT_BUSY);
    repair_kernel(directory, id);
    metadata = parse(read_file(directory / "record.json", 65536));
    std::vector<Entry> entries = collect(directory);
    int64_t serial = -1;
    if (fs::is_regular_file(directory / "assets/guest.itpack")) {
        try {
            serial = guest_package_serial(directory / "assets/guest.itpack", board, build);
        } catch (...) {
        }
    }
    uint64_t total = 0, data = 0;
    std::string files;
    for (const auto &e : entries) {
        total += e.size;
        data += e.data;
        files += std::string(files.empty() ? "" : ",") + "\n  {\"name\":" + quote(e.name) +
                 ",\"size\":" + std::to_string(e.size) + ",\"data\":" + std::to_string(e.data) + "}";
    }
    Entry manifest;
    manifest.name = manifest_name;
    manifest.bytes = std::string("{\"format\":\"") + format_name + "\",\"version\":" + std::to_string(format_version) +
                     ",\"app\":" + quote(c->app && *c->app ? c->app : "LightTouch") + ",\"exported\":" + quote(iso_now()) +
                     ",\"name\":" + quote(name) + ",\"board\":" + quote(board) + ",\"build\":" + quote(build) +
                     ",\"base_sha256\":" + quote(metadata.at("base_sha256").string()) +
                     ",\"guest_tools_serial\":" + std::to_string(serial) +
                     ",\"unclean\":" + (fs::exists(directory / "state/.dirty") ? "true" : "false") +
                     ",\"bytes\":" + std::to_string(total) + ",\"data_bytes\":" + std::to_string(data) +
                     ",\"files\":[" + files + "\n]}\n";
    manifest.size = manifest.data = manifest.bytes.size();
    entries.insert(entries.begin(), std::move(manifest));
    observer.begin("exporting", total);
    // A named file is written beside its destination and renamed into place once complete.
    fs::path partial;
    OwnedFd owned;
    int fd = c->fd;
    if (c->path && *c->path) {
        partial = fs::path(c->path).concat(".partial");
        std::error_code ec;
        fs::remove(partial, ec);
        owned.reset(lt::os::open(partial.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0644));
        io(owned.n >= 0, "create " + partial.string());
        fd = owned.n;
    }
    require(fd >= 0, "export destination missing");
    try {
        Exporter(entries, fd, observer).run();
        // Last chance to cancel, before the file takes its name.
        observer.done = observer.total;
        observer.report(true);
        if (!partial.empty()) {
            io(lt::os::fsync(fd) == 0, "sync device file");
            owned.reset();
            io(lt::os::rename(partial.c_str(), c->path) == 0, "save device file");
        }
    } catch (...) {
        if (!partial.empty()) {
            owned.reset();
            std::error_code ec;
            fs::remove(partial, ec);
        }
        throw;
    }
}

// ---- import ----

struct In {
    int fd;
    std::vector<uint8_t> buffer = std::vector<uint8_t>(1u << 20);
    size_t at = 0, end = 0;
    bool eof = false;
    bool fill() {
        if (at < end)
            return true;
        if (eof)
            return false;
        for (;;) {
            ssize_t n = lt::os::read(fd, buffer.data(), buffer.size());
            if (n < 0 && errno == EINTR)
                continue;
            io(n >= 0, "read device file");
            at = 0;
            end = size_t(n);
            eof = n == 0;
            return n > 0;
        }
    }
    void exact(void *destination, size_t n) {
        auto *p = static_cast<uint8_t *>(destination);
        while (n) {
            require(fill(), std::string(damaged) + " (it ends early: was the copy cut short?)");
            const size_t k = std::min(n, end - at);
            std::memcpy(p, buffer.data() + at, k);
            at += k;
            p += k;
            n -= k;
        }
    }
    uint16_t u16() {
        uint8_t b[2];
        exact(b, 2);
        return uint16_t(b[0] | b[1] << 8);
    }
    uint32_t u32() {
        uint8_t b[4];
        exact(b, 4);
        return uint32_t(b[0]) | uint32_t(b[1]) << 8 | uint32_t(b[2]) << 16 | uint32_t(b[3]) << 24;
    }
    uint64_t u64() {
        const uint64_t low = u32();
        return low | uint64_t(u32()) << 32;
    }
};

struct Sink {
    virtual void put(const uint8_t *p, size_t n) = 0;
    virtual ~Sink() = default;
};

constexpr uint64_t unknown_size = UINT64_MAX;
// One entry's bytes, inflated into [sink]; checks its data descriptor (CRC-32 and sizes) against what arrived.
void read_entry(In &in, Sink &sink, uint64_t expected, Observer *observer) {
    z_stream z{};
    require(inflateInit2(&z, -15) == Z_OK, "inflate setup", LT_IO);
    struct End {
        z_stream &z;
        ~End() { inflateEnd(&z); }
    } end{z};
    std::vector<uint8_t> output(chunk_bytes);
    uint64_t produced = 0, compressed = 0;
    uint32_t crc = 0;
    for (;;) {
        if (in.at == in.end)
            in.fill();
        z.next_in = in.buffer.data() + in.at;
        z.avail_in = uInt(in.end - in.at);
        z.next_out = output.data();
        z.avail_out = uInt(output.size());
        const int r = inflate(&z, Z_NO_FLUSH);
        const size_t used = (in.end - in.at) - z.avail_in;
        in.at += used;
        compressed += used;
        require(r != Z_BUF_ERROR || in.at < in.end || !in.eof,
                std::string(damaged) + " (it ends early: was the copy cut short?)");
        require(r == Z_OK || r == Z_STREAM_END || r == Z_BUF_ERROR, damaged);
        const size_t n = output.size() - z.avail_out;
        require(expected == unknown_size || produced + n <= expected, damaged);
        if (n) {
            crc = uint32_t(crc32(crc, output.data(), uInt(n)));
            sink.put(output.data(), n);
            produced += n;
            if (observer)
                observer->add(n);
        }
        if (r == Z_STREAM_END)
            break;
    }
    uint32_t signature = in.u32();
    const uint32_t stored_crc = signature == descriptor_signature ? in.u32() : signature;
    const uint64_t stored_compressed = in.u64(), stored_size = in.u64();
    require((expected == unknown_size || produced == expected) && stored_size == produced &&
                stored_compressed == compressed && stored_crc == crc,
            damaged);
}

struct LocalHeader {
    std::string name;
    bool central = false, ours = false;  // ours: deflated, sizes in a data descriptor, as an export writes them
};

LocalHeader read_header(In &in) {
    LocalHeader h;
    const uint32_t signature = in.u32();
    if (signature == central_signature) {
        h.central = true;
        return h;
    }
    require(signature == local_signature, damaged);
    in.u16();  // version needed
    const uint16_t flags = in.u16(), method = in.u16();
    in.u32();  // time, date
    in.u32();  // CRC-32
    in.u32();
    in.u32();  // sizes
    const uint16_t name_length = in.u16(), extra_length = in.u16();
    h.name.resize(name_length);
    in.exact(h.name.data(), name_length);
    std::vector<uint8_t> extra(extra_length);
    in.exact(extra.data(), extra_length);
    h.ours = (flags & 0x08) && method == 8;
    return h;
}

LocalHeader read_entry_header(In &in) {
    LocalHeader h = read_header(in);
    require(h.central || h.ours, "This device file was changed by another program; export it again");
    return h;
}

struct StringSink : Sink {
    std::string text;
    void put(const uint8_t *p, size_t n) override {
        require(text.size() + n <= (16u << 20), damaged);
        text.append(reinterpret_cast<const char *>(p), n);
    }
};

// A file of the new record: zeros become holes, its length is set at the end, and a base file is hashed as it goes.
struct FileSink : Sink {
    int fd;
    uint64_t at = 0;
    Sha256 *hash;
    FileSink(int fd, Sha256 *hash) : fd(fd), hash(hash) {}
    void put(const uint8_t *p, size_t n) override {
        if (hash)
            hash->update(p, n);
        // Runs of data are written; 4 KiB pages of zeros (a qcow2's punched clusters, a NAND's blank pages) left as
        // holes.
        constexpr size_t page = 4096;
        size_t run = 0;  // start of the pending run of data
        for (size_t i = 0, k; i < n; i += k) {
            k = std::min(page - size_t((at + i) % page), n - i);  // pages of the file, not of this block
            if (all_zero(p + i, k)) {
                if (run < i)
                    write_run(p + run, i - run, at + run);
                run = i + k;
            }
        }
        if (run < n)
            write_run(p + run, n - run, at + run);
        at += n;
    }
    void write_run(const uint8_t *p, size_t n, uint64_t offset) {
        for (size_t done = 0; done < n;) {
            ssize_t k = lt::os::pwrite(fd, p + done, n - done, int64_t(offset + done));
            if (k < 0 && errno == EINTR)
                continue;
            if (k < 0 && errno == ENOSPC)
                throw Failure(LT_IO, "There isn't enough free space for this device");
            io(k > 0, "write device");
            done += size_t(k);
        }
    }
};

// Hands an entry's bytes to a writer thread (hashing, finding holes, writing), so inflating the next ones overlaps
// it: an import is then about as fast as inflate alone.
class Pipe {
public:
    Pipe() : worker([this] { run(); }) {}
    ~Pipe() {
        {
            std::lock_guard<std::mutex> lock(guard);
            closing = true;
        }
        wake.notify_all();
        worker.join();
    }
    void push(Sink *target, const uint8_t *p, size_t n) {
        std::unique_lock<std::mutex> lock(guard);
        wake.wait(lock, [&] { return failure || queue.size() < 8; });
        if (failure)
            std::rethrow_exception(failure);
        std::vector<uint8_t> block;
        if (!spare.empty()) {
            block = std::move(spare.back());
            spare.pop_back();
        }
        block.assign(p, p + n);
        queue.emplace_back(target, std::move(block));
        lock.unlock();
        wake.notify_all();
    }
    // Drop what is queued and wait out the block being written: the sinks it names are about to go.
    void abandon() {
        std::unique_lock<std::mutex> lock(guard);
        queue.clear();
        wake.wait(lock, [&] { return !busy; });
    }
    // Everything pushed so far written (or its failure thrown).
    void drain() {
        std::unique_lock<std::mutex> lock(guard);
        wake.wait(lock, [&] { return failure || (queue.empty() && !busy); });
        if (failure)
            std::rethrow_exception(failure);
    }

private:
    std::mutex guard;
    std::condition_variable wake;
    std::deque<std::pair<Sink *, std::vector<uint8_t>>> queue;
    std::vector<std::vector<uint8_t>> spare;
    bool busy = false, closing = false;
    std::exception_ptr failure;
    std::thread worker;
    void run() {
        std::unique_lock<std::mutex> lock(guard);
        for (;;) {
            wake.wait(lock, [&] { return closing || !queue.empty(); });
            if (queue.empty())
                return;
            auto item = std::move(queue.front());
            queue.pop_front();
            busy = true;
            lock.unlock();
            wake.notify_all();
            std::exception_ptr error;
            try {
                item.first->put(item.second.data(), item.second.size());
            } catch (...) {
                error = std::current_exception();
            }
            lock.lock();
            busy = false;
            if (error && !failure) {
                failure = error;
                queue.clear();
            }
            spare.push_back(std::move(item.second));
            lock.unlock();
            wake.notify_all();
            lock.lock();
        }
    }
};
struct Forward : Sink {
    Pipe &pipe;
    Sink &target;
    Forward(Pipe &pipe, Sink &target) : pipe(pipe), target(target) {}
    void put(const uint8_t *p, size_t n) override { pipe.push(&target, p, n); }
};

void sync_files(const std::vector<fs::path> &files) {
    std::atomic<size_t> next{0};
    std::mutex guard;
    std::exception_ptr failure;
    auto work = [&] {
        for (size_t i; (i = next++) < files.size();) {
            try {
                OwnedFd fd(lt::os::open(files[i].c_str(), O_RDWR | O_CLOEXEC));
                io(fd.n >= 0 && lt::os::fsync(fd.n) == 0, "sync " + files[i].string());
            } catch (...) {
                std::lock_guard<std::mutex> lock(guard);
                if (!failure)
                    failure = std::current_exception();
            }
        }
    };
    std::vector<std::thread> pool;
    for (size_t t = 1; t < std::min<size_t>(8, files.size()); ++t)
        pool.emplace_back(work);
    work();
    for (auto &t : pool)
        t.join();
    if (failure)
        std::rethrow_exception(failure);
}

std::string unique_name(lt_library *l, const std::string &wanted) {
    std::set<std::string> taken;
    for (const auto &id : records(l)) {
        try {
            taken.insert(parse(read_file(l->root / "records" / id / "record.json", 65536)).at("name").string());
        } catch (...) {
        }
    }
    if (!taken.count(wanted))
        return wanted;
    // "Name (2)" imported again becomes "Name (3)", not "Name (2) (2)".
    std::string stem = wanted;
    const size_t open = stem.rfind(" (");
    if (open != std::string::npos && stem.back() == ')' && open + 3 < stem.size() &&
        stem.find_first_not_of("0123456789", open + 2) == stem.size() - 1)
        stem.erase(open);
    for (int n = 2;; ++n) {
        const std::string suffix = " (" + std::to_string(n) + ")";
        std::string base = stem;
        while (base.size() + suffix.size() > 127) {
            base.pop_back();
            while (!base.empty() && (static_cast<unsigned char>(base.back()) & 0xc0) == 0x80)
                base.pop_back();
            if (!base.empty() && static_cast<unsigned char>(base.back()) >= 0xc0)
                base.pop_back();
        }
        if (!taken.count(base + suffix))
            return base + suffix;
    }
}

// Stages an import left behind when its process died: each import holds a lock on its stage's .owner.
void sweep_stages(lt_library *l) {
    std::vector<fs::path> stale;
    std::error_code ec;
    for (const auto &e : fs::directory_iterator(l->root / "records", ec)) {
        if (e.path().filename().string().rfind(".record-import-", 0) != 0)
            continue;
        const fs::path owner = e.path() / ".owner";
        if (!fs::exists(owner, ec)) {
            // Made, but not yet claimed: only an old one is abandoned.
            const auto age = fs::file_time_type::clock::now() - fs::last_write_time(e.path(), ec);
            if (!ec && age > std::chrono::minutes(10))
                stale.push_back(e.path());
            continue;
        }
        OwnedFd fd(lt::os::open(owner.c_str(), O_RDWR | O_CLOEXEC | O_NOFOLLOW));
        if (fd.n >= 0 && lt::os::lock_exclusive(fd.n) == 0)
            stale.push_back(e.path());
    }
    for (const auto &p : stale)
        discard(p);
}

void import_device(lt_library *l, const lt_transfer_import_config *c, Observer &observer, lt_record_info *out) {
    abi(c);
    abi(out);
    OwnedFd owned;
    int fd = c->fd;
    if (c->path && *c->path) {
        owned.reset(lt::os::open(c->path, O_RDONLY | O_CLOEXEC));
        io(owned.n >= 0, "open " + std::string(c->path));
        fd = owned.n;
    }
    require(fd >= 0, "device file missing");
    In in{fd};
    sweep_stages(l);
    // The manifest first: nothing is written for a file this build can't take.
    LocalHeader first = read_header(in);
    require(!first.central && first.name == manifest_name, "This isn't a device exported by LightTouch",
            LT_UNSUPPORTED);
    require(first.ours, "This device file was changed by another program; export it again");
    StringSink manifest_text;  // bounded by the sink: its size isn't known until it is read
    read_entry(in, manifest_text, unknown_size, nullptr);
    Json manifest = parse(manifest_text.text);
    require(manifest.kind == Json::Object && manifest.find("format") && manifest.at("format").kind == Json::String &&
                manifest.at("format").string() == format_name,
            "This isn't a device exported by LightTouch", LT_UNSUPPORTED);
    const int64_t version = manifest.at("version").integer();
    require(version >= 1, damaged);
    require(version <= format_version,
            "This device was exported by a newer LightTouch (" + (manifest.find("app") ? manifest.at("app").string() : "") +
                "); update LightTouch to import it",
            LT_UNSUPPORTED);
    const auto &list = manifest.at("files");
    require(list.kind == Json::Array && list.array.size() <= 1000000, damaged);
    std::map<std::string, uint64_t> sizes;
    uint64_t total = 0, data = 0;
    for (const auto &f : list.array) {
        const std::string name = f.at("name").string();
        require(entry_name_ok(name), damaged);
        const int64_t size = f.at("size").integer(), allocated = f.at("data").integer();
        require(size >= 0 && allocated >= 0 && uint64_t(size) <= (256ull << 30), damaged);
        require(sizes.emplace(name, uint64_t(size)).second, damaged);
        total += uint64_t(size);
        data += uint64_t(allocated);
    }
    require(sizes.count("record.json") && sizes.count("base/device.lock.json"), damaged);
    for (const auto &entry : sizes)
        for (size_t p = entry.first.find('/'); p != std::string::npos; p = entry.first.find('/', p + 1))
            require(!sizes.count(entry.first.substr(0, p)), damaged);
    {
        std::error_code ec;
        const auto space = fs::space(l->root, ec);
        const uint64_t need = data + (256ull << 20);
        if (!ec)
            require(space.available >= need,
                    "There isn't enough free space for this device: it needs " +
                        std::to_string((need + (1ull << 29)) >> 30) + " GB, and " +
                        std::to_string(space.available >> 30) + " GB are free",
                    LT_IO);
    }
    // The stage: hidden from the record list, claimed by a lock on its .owner while this import runs.
    std::string pattern = (l->root / "records/.record-import-XXXXXX").string();
    std::vector<char> tmp(pattern.begin(), pattern.end());
    tmp.push_back(0);
    io(lt::os::mkdtemp(tmp.data()) != nullptr, "device import staging");
    const fs::path stage = tmp.data();
    OwnedFd owner(lt::os::open((stage / ".owner").c_str(), O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600));
    try {
        io(owner.n >= 0, "device import staging");
        require(lt::os::lock_exclusive(owner.n) == 0, "device import staging", LT_BUSY);
        observer.begin("importing", total);
        std::set<std::string> seen;
        std::map<std::string, std::string> digests;  // base files, by name relative to base/
        Pipe pipe;
        std::vector<fs::path> unsynced;
        for (;;) {
            LocalHeader h = read_entry_header(in);
            if (h.central)
                break;
            const auto size = sizes.find(h.name);
            require(size != sizes.end() && seen.insert(h.name).second, damaged);
            const fs::path path = stage / fs::u8path(h.name);
            fs::create_directories(path.parent_path());
            OwnedFd file(lt::os::open(path.c_str(), O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC, 0600));
            io(file.n >= 0, "create " + h.name);
            const bool base = h.name.rfind("base/", 0) == 0;
            Sha256 hash;
            FileSink sink(file.n, base ? &hash : nullptr);
            Forward forward(pipe, sink);
            try {
                read_entry(in, forward, size->second, &observer);
                pipe.drain();
            } catch (...) {
                pipe.abandon();
                throw;
            }
            io(lt::os::ftruncate(file.n, int64_t(size->second)) == 0, "size " + h.name);
            // A large file is synced now; small ones (a 2G's thousands of NAND page files) together at the end, on
            // several threads: one at a time, Android's per-file syncs took minutes.
            if (size->second >= (1u << 20))
                io(lt::os::fsync(file.n) == 0, "sync " + h.name);
            else
                unsynced.push_back(path);
            file.reset();
            if (base)
                digests.emplace(h.name.substr(5), hash.finish());
            if (h.name == "base/device.lock.json")
                check_boot_route(parse(read_file(path)));
        }
        require(seen.size() == sizes.size(), damaged);
        sync_files(unsynced);
        observer.begin("finishing", 0);
        // The record's own pins: its lock, boot ROM and guest tools are the ones it names.
        auto metadata = parse(read_file(stage / "record.json", 65536));
        auto lock = parse(read_file(stage / "base/device.lock.json"));
        require(metadata.at("schema").integer() == 1 && metadata.at("board").string() == lock.at("board").string() &&
                    metadata.at("build").string() == lock.at("build").string(),
                damaged);
        require(metadata.at("base_sha256").string() == digests.at("device.lock.json"), damaged);
        for (const auto &asset : {std::make_pair("bootrom_sha256", "assets/bootrom.bin"),
                                  std::make_pair("guest_package_sha256", "assets/guest.itpack")}) {
            const Json *pin = metadata.find(asset.first);
            const bool present = fs::exists(stage / asset.second);
            require(bool(pin) == present, damaged);
            if (pin)
                require(digest_file(stage / asset.second) == pin->string(), damaged);
        }
        fs::create_directories(stage / "state");
        // The base against its lock, with the digests taken as it arrived (throws on any mismatch).
        try {
            seed_admission(stage / "base", stage / "state", digests);
        } catch (const Failure &f) {
            throw Failure(f.status == LT_UNSUPPORTED ? LT_UNSUPPORTED : LT_INVALID,
                          f.status == LT_UNSUPPORTED ? f.what() : std::string(damaged) + " (" + f.what() + ")");
        }
        for (auto it = fs::recursive_directory_iterator(stage); it != fs::recursive_directory_iterator(); ++it)
            if (it->is_directory())
                sync_dir(it->path());
        sync_dir(stage);
        // Publish: a new id (pairing and network folders elsewhere are keyed by id), its name made unique.
        auto held = wait_lock(l);
        std::string id;
        do
            id = new_id();
        while (fs::exists(l->root / "records" / id));
        const std::string name = unique_name(l, metadata.at("name").string());
        display_name(name);
        atomic_write(stage / "record.json", record_json(metadata, id, name));
        owner.reset();
        fs::remove(stage / ".owner");
        fs::rename(stage, l->root / "records" / id);
        sync_dir(l->root / "records");
        const fs::path directory = l->root / "records" / id;
        // The admission cache names files by path: written again for where they are now (no file is read).
        try {
            seed_admission(directory / "base", directory / "state", digests);
        } catch (...) {
        }
        if (selected(l).empty())
            atomic_write(l->root / "selected", id);
        describe(l, id, out);
    } catch (...) {
        owner.reset();
        discard(stage);
        throw;
    }
}

int no_observer(void *, const char *, uint64_t, uint64_t) { return 0; }
}  // namespace
}  // namespace lt

extern "C" {
lt_status lt_library_export(lt_library *l, const lt_export_config *c, lt_import_phase_fn observer, void *opaque,
                            lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(l, "null library");
        lt::Observer o{observer ? observer : lt::no_observer, opaque, "", 0, 0, {}};
        lt::export_record(l, c, o);
    });
}
lt_status lt_library_import_device(lt_library *l, const lt_transfer_import_config *c, lt_import_phase_fn observer,
                                   void *opaque, lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(l, "null library");
        lt::Observer o{observer ? observer : lt::no_observer, opaque, "", 0, 0, {}};
        lt::import_device(l, c, o, out);
    });
}
}
