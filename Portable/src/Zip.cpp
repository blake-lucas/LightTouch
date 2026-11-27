#include "Zip.hpp"
#include "Os.hpp"
#include <zlib.h>
#include <algorithm>
#include <atomic>
#include <ctime>
#include <fcntl.h>

namespace lt::zip {
namespace {
constexpr uint32_t local_signature = 0x04034b50, central_signature = 0x02014b50, descriptor_signature = 0x08074b50;
constexpr uint32_t index_magic = 0x435a544c; // "LTZC"
constexpr uint32_t zero_flag = 0x80000000u;
constexpr const char *index_name = "chunks.bin";
constexpr const char *damaged = "This save state is damaged or incomplete";

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
uint32_t get16(const uint8_t *p) { return uint32_t(p[0]) | uint32_t(p[1]) << 8; }
uint32_t get32(const uint8_t *p) { return get16(p) | get16(p + 2) << 16; }
uint64_t get64(const uint8_t *p) { return uint64_t(get32(p)) | uint64_t(get32(p + 4)) << 32; }

void write_fully(int fd, const uint8_t *p, size_t n) {
    while (n) {
        ssize_t k = lt::os::write(fd, p, n);
        if (k < 0 && errno == EINTR)
            continue;
        if (k < 0 && errno == ENOSPC)
            throw Failure(LT_IO, "There isn't enough free space to save the state");
        io(k > 0, "write save state");
        p += k;
        n -= size_t(k);
    }
}

void read_fully(int fd, uint8_t *p, size_t n, uint64_t offset) {
    while (n) {
        ssize_t k = lt::os::pread(fd, p, n, int64_t(offset));
        if (k < 0 && errno == EINTR)
            continue;
        io(k >= 0, "read save state");
        require(k > 0, damaged, LT_IO);
        p += k;
        n -= size_t(k);
        offset += uint64_t(k);
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

// One chunk as a sync-flushed raw deflate segment: byte-aligned and free of back-references to earlier chunks, so
// segments made apart concatenate into one stream, and each inflates on its own.
struct Deflater {
    z_stream z{};
    Deflater() { require(deflateInit2(&z, 1, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY) == Z_OK, "deflate setup", LT_IO); }
    ~Deflater() { deflateEnd(&z); }
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

struct Inflater {
    z_stream z{};
    Inflater() { require(inflateInit2(&z, -15) == Z_OK, "inflate setup", LT_IO); }
    ~Inflater() { inflateEnd(&z); }
    // One segment the Deflater made into exactly n bytes.
    void chunk(const uint8_t *in, size_t in_size, uint8_t *out, size_t n) {
        require(inflateReset(&z) == Z_OK, "inflate reset", LT_IO);
        z.next_in = const_cast<Bytef *>(in);
        z.avail_in = uInt(in_size);
        z.next_out = out;
        z.avail_out = uInt(n);
        const int r = inflate(&z, Z_SYNC_FLUSH);
        require((r == Z_OK || r == Z_STREAM_END || r == Z_BUF_ERROR) && z.avail_out == 0, damaged, LT_IO);
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

unsigned thread_count() { return std::max(1u, std::min(8u, std::thread::hardware_concurrency())); }

struct Fd {
    int n = -1;
    explicit Fd(int fd) : n(fd) {}
    Fd(const Fd &) = delete;
    Fd &operator=(const Fd &) = delete;
    ~Fd() {
        if (n >= 0)
            lt::os::close(n);
    }
};
} // namespace

// ---- writer ----

struct Writer::Central {
    std::string name;
    uint32_t crc = 0;
    uint64_t compressed = 0, size = 0, offset = 0;
    std::vector<uint32_t> chunks;
};

struct Writer::Item {
    enum Kind { header, chunk, trailer } kind;
    std::string name;
    std::vector<uint8_t> data; // a stream chunk's bytes
    fs::path path;             // or a file's range
    uint64_t offset = 0, length = 0;
    bool hole = false;
};

struct Writer::Pending {
    Item item;
    bool claimed = false, ready = false, zero = false;
    std::vector<uint8_t> output;
    uint32_t crc = 0;
};

Writer::Writer(int fd, Progress progress) : fd(fd), progress(std::move(progress)), central(new std::vector<Central>) {
    dos_time(time, date);
    const unsigned threads = thread_count();
    window = threads * 3;
    for (unsigned t = 0; t < threads; ++t)
        pool.emplace_back([this] { work(); });
    writer = std::thread([this] { output(); });
}

Writer::~Writer() {
    {
        std::lock_guard<std::mutex> lock(guard);
        closing = true;
        if (!failure)
            failure = std::make_exception_ptr(Failure(LT_CANCELLED, "cancelled"));
    }
    wake.notify_all();
    for (auto &t : pool)
        if (t.joinable())
            t.join();
    if (writer.joinable())
        writer.join();
    delete central;
}

void Writer::fail(std::exception_ptr e) {
    std::lock_guard<std::mutex> lock(guard);
    if (!failure)
        failure = e;
    wake.notify_all();
}

void Writer::rethrow() {
    std::lock_guard<std::mutex> lock(guard);
    if (failure)
        std::rethrow_exception(failure);
}

void Writer::submit(Item item) {
    auto pending = std::make_shared<Pending>();
    const bool chunk_item = item.kind == Item::chunk;
    pending->item = std::move(item);
    if (!chunk_item)
        pending->ready = pending->claimed = true;
    std::unique_lock<std::mutex> lock(guard);
    wake.wait(lock, [&] { return failure || queue.size() < window * 2; });
    if (failure)
        std::rethrow_exception(failure);
    queue.push_back(std::move(pending));
    lock.unlock();
    wake.notify_all();
}

void Writer::begin(const std::string &name) {
    require(!in_entry, "zip entry already open");
    in_entry = true;
    chunk.clear();
    chunk.reserve(chunk_bytes);
    submit({Item::header, name, {}, {}, 0, 0, false});
}

void Writer::flush_chunk() {
    if (chunk.empty())
        return;
    Item item{Item::chunk, {}, std::move(chunk), {}, 0, 0, false};
    item.length = item.data.size();
    submit(std::move(item));
    chunk = {};
    chunk.reserve(chunk_bytes);
}

void Writer::write(const void *data, size_t size) {
    require(in_entry, "no zip entry open");
    auto *p = static_cast<const uint8_t *>(data);
    while (size) {
        const size_t n = std::min(size, chunk_bytes - chunk.size());
        chunk.insert(chunk.end(), p, p + n);
        p += n;
        size -= n;
        if (chunk.size() == chunk_bytes)
            flush_chunk();
    }
}

void Writer::end() {
    require(in_entry, "no zip entry open");
    flush_chunk();
    in_entry = false;
    submit({Item::trailer, {}, {}, {}, 0, 0, false});
}

void Writer::file(const std::string &name, const fs::path &path) {
    Fd in(lt::os::open(path.c_str(), O_RDONLY | O_CLOEXEC));
    io(in.n >= 0, "open " + path.string());
    lt::os::Stat st{};
    io(lt::os::fstat(in.n, &st) == 0, "inspect " + path.string());
    const uint64_t size = uint64_t(st.st_size);
    const auto ranges = size ? file_data_ranges(in.n, int64_t(size)) : std::vector<std::pair<int64_t, int64_t>>{};
    require(!in_entry, "zip entry already open");
    submit({Item::header, name, {}, {}, 0, 0, false});
    for (uint64_t at = 0; at < size; at += chunk_bytes) {
        const uint64_t length = std::min<uint64_t>(chunk_bytes, size - at);
        bool hole = true;
        for (const auto &r : ranges)
            if (uint64_t(r.first) < at + length && uint64_t(r.second) > at) {
                hole = false;
                break;
            }
        submit({Item::chunk, {}, {}, path, at, length, hole});
    }
    submit({Item::trailer, {}, {}, {}, 0, 0, false});
}

void Writer::work() {
    Deflater deflater;
    std::vector<uint8_t> buffer(chunk_bytes);
    for (;;) {
        std::shared_ptr<Pending> job;
        {
            std::unique_lock<std::mutex> lock(guard);
            wake.wait(lock, [&] {
                if (failure || closing)
                    return true;
                size_t seen = 0;
                for (auto &p : queue) {
                    if (++seen > window)
                        break;
                    if (!p->claimed)
                        return true;
                }
                return false;
            });
            if (failure || closing)
                return;
            size_t seen = 0;
            for (auto &p : queue) {
                if (++seen > window)
                    break;
                if (!p->claimed) {
                    job = p;
                    p->claimed = true;
                    break;
                }
            }
            if (!job)
                continue;
        }
        try {
            const Item &item = job->item;
            const uint8_t *data = item.data.data();
            if (!item.path.empty()) {
                data = buffer.data();
                if (item.hole) {
                    std::fill(buffer.begin(), buffer.begin() + std::ptrdiff_t(item.length), uint8_t(0));
                } else {
                    Fd in(lt::os::open(item.path.c_str(), O_RDONLY | O_CLOEXEC));
                    io(in.n >= 0, "open " + item.path.string());
                    for (uint64_t done = 0; done < item.length;) {
                        ssize_t n = lt::os::pread(in.n, buffer.data() + done, size_t(item.length - done),
                                                  int64_t(item.offset + done));
                        if (n < 0 && errno == EINTR)
                            continue;
                        io(n >= 0, "read " + item.path.string());
                        require(n > 0, item.path.string() + " changed while it was saved", LT_IO);
                        done += uint64_t(n);
                    }
                }
            }
            job->zero = item.hole || all_zero(data, size_t(item.length));
            job->output = deflater.chunk(data, size_t(item.length));
            job->crc = uint32_t(crc32(0, data, uInt(item.length)));
            job->item.data = {};
        } catch (...) {
            fail(std::current_exception());
            return;
        }
        {
            std::lock_guard<std::mutex> lock(guard);
            job->ready = true;
        }
        wake.notify_all();
    }
}

void Writer::output() {
    Central current;
    uint64_t start = 0;
    try {
        for (;;) {
            std::shared_ptr<Pending> next;
            {
                std::unique_lock<std::mutex> lock(guard);
                wake.wait(lock, [&] { return failure || closing || (!queue.empty() && queue.front()->ready); });
                if (failure)
                    return;
                if (queue.empty() || !queue.front()->ready) {
                    if (closing)
                        return;
                    continue;
                }
                next = queue.front();
            }
            const Item &item = next->item;
            if (item.kind == Item::header) {
                current = Central{};
                current.name = item.name;
                current.offset = out_offset;
                std::vector<uint8_t> h;
                put32(h, local_signature);
                put16(h, 45);     // version needed: ZIP64
                put16(h, 0x0808); // sizes in the data descriptor; UTF-8 name
                put16(h, 8);      // deflate
                put16(h, time);
                put16(h, date);
                put32(h, 0);
                put32(h, 0xffffffff); // sizes: in the ZIP64 field, then the descriptor
                put32(h, 0xffffffff);
                put16(h, uint16_t(item.name.size()));
                put16(h, 20);
                h.insert(h.end(), item.name.begin(), item.name.end());
                put16(h, 1);
                put16(h, 16);
                put64(h, 0);
                put64(h, 0);
                write_fully(fd, h.data(), h.size());
                out_offset += h.size();
                start = out_offset;
            } else if (item.kind == Item::chunk) {
                write_fully(fd, next->output.data(), next->output.size());
                out_offset += next->output.size();
                current.crc = uint32_t(crc32_combine(current.crc, next->crc, static_cast<z_off_t>(item.length)));
                current.size += item.length;
                current.chunks.push_back(uint32_t(next->output.size()) | (next->zero ? zero_flag : 0));
                if (progress)
                    progress(item.length);
            } else {
                static const uint8_t final_block[2] = {0x03, 0x00};
                write_fully(fd, final_block, 2);
                out_offset += 2;
                current.compressed = out_offset - start;
                std::vector<uint8_t> d;
                put32(d, descriptor_signature);
                put32(d, current.crc);
                put64(d, current.compressed);
                put64(d, current.size);
                write_fully(fd, d.data(), d.size());
                out_offset += d.size();
                central->push_back(std::move(current));
                current = Central{};
            }
            {
                std::lock_guard<std::mutex> lock(guard);
                queue.pop_front();
            }
            wake.notify_all();
        }
    } catch (...) {
        fail(std::current_exception());
    }
}

void Writer::finish() {
    require(!in_entry, "zip entry still open");
    // The chunk index: every entry so far, once the queue has drained into central.
    auto drain = [&] {
        std::unique_lock<std::mutex> lock(guard);
        wake.wait(lock, [&] { return failure || queue.empty(); });
        if (failure)
            std::rethrow_exception(failure);
    };
    drain();
    std::vector<uint8_t> index;
    put32(index, index_magic);
    put32(index, 1);
    put32(index, uint32_t(central->size()));
    for (const auto &c : *central) {
        put16(index, uint32_t(c.name.size()));
        index.insert(index.end(), c.name.begin(), c.name.end());
        put32(index, uint32_t(c.chunks.size()));
        for (uint32_t k : c.chunks)
            put32(index, k);
    }
    begin(index_name);
    write(index.data(), index.size());
    end();
    drain();
    {
        std::lock_guard<std::mutex> lock(guard);
        closing = true;
    }
    wake.notify_all();
    for (auto &t : pool)
        t.join();
    writer.join();
    rethrow();

    const uint64_t directory = out_offset;
    std::vector<uint8_t> h;
    for (const auto &c : *central) {
        std::vector<uint8_t> extra;
        const bool big_size = c.size >= 0xffffffffu, big_compressed = c.compressed >= 0xffffffffu,
                   big_offset = c.offset >= 0xffffffffu;
        if (big_size)
            put64(extra, c.size);
        if (big_compressed)
            put64(extra, c.compressed);
        if (big_offset)
            put64(extra, c.offset);
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
        put16(h, 0); // comment
        put16(h, 0); // disk
        put16(h, 0); // internal attributes
        put32(h, 0); // external attributes
        put32(h, big_offset ? 0xffffffffu : uint32_t(c.offset));
        h.insert(h.end(), c.name.begin(), c.name.end());
        if (!extra.empty()) {
            put16(h, 1);
            put16(h, uint16_t(extra.size()));
            h.insert(h.end(), extra.begin(), extra.end());
        }
    }
    const uint64_t directory_size = h.size(), end64 = directory + directory_size;
    put32(h, 0x06064b50); // ZIP64 end of central directory
    put64(h, 44);
    put16(h, 45);
    put16(h, 45);
    put32(h, 0);
    put32(h, 0);
    put64(h, central->size());
    put64(h, central->size());
    put64(h, directory_size);
    put64(h, directory);
    put32(h, 0x07064b50); // its locator
    put32(h, 0);
    put64(h, end64);
    put32(h, 1);
    put32(h, 0x06054b50); // end of central directory
    put16(h, 0);
    put16(h, 0);
    put16(h, uint16_t(std::min<size_t>(central->size(), 0xffff)));
    put16(h, uint16_t(std::min<size_t>(central->size(), 0xffff)));
    put32(h, uint32_t(std::min<uint64_t>(directory_size, 0xffffffffu)));
    put32(h, uint32_t(std::min<uint64_t>(directory, 0xffffffffu)));
    put16(h, 0);
    write_fully(fd, h.data(), h.size());
    out_offset += h.size();
}

// ---- reader ----

Reader::Reader(int fd) : fd(fd) {
    lt::os::Stat st{};
    io(lt::os::fstat(fd, &st) == 0, "inspect save state");
    const uint64_t size = uint64_t(st.st_size);
    require(size >= 22 + 20 + 56, damaged, LT_IO);
    // Our writer ends with the ZIP64 record (56), its locator (20) and the end record (22), with no comment.
    uint8_t tail[98];
    read_fully(fd, tail, sizeof tail, size - sizeof tail);
    require(get32(tail + 76) == 0x06054b50 && get32(tail + 56) == 0x07064b50, damaged, LT_IO);
    const uint64_t end64 = get64(tail + 56 + 8);
    require(end64 + 56 + 20 + 22 == size && get32(tail) == 0x06064b50, damaged, LT_IO);
    const uint64_t count = get64(tail + 32), directory_size = get64(tail + 40), directory = get64(tail + 48);
    require(count < 4096 && directory_size < (16u << 20) && directory + directory_size == end64, damaged, LT_IO);
    std::vector<uint8_t> d(static_cast<size_t>(directory_size));
    read_fully(fd, d.data(), d.size(), directory);
    size_t at = 0;
    for (uint64_t i = 0; i < count; ++i) {
        require(at + 46 <= d.size() && get32(&d[at]) == central_signature && get16(&d[at + 10]) == 8, damaged, LT_IO);
        Entry e;
        e.crc = get32(&d[at + 16]);
        e.compressed = get32(&d[at + 20]);
        e.size = get32(&d[at + 24]);
        const size_t name = get16(&d[at + 28]), extra = get16(&d[at + 30]), comment = get16(&d[at + 32]);
        e.offset = get32(&d[at + 42]);
        require(at + 46 + name + extra + comment <= d.size(), damaged, LT_IO);
        e.name.assign(reinterpret_cast<const char *>(&d[at + 46]), name);
        for (size_t x = at + 46 + name; x + 4 <= at + 46 + name + extra;) {
            const uint32_t id = get16(&d[x]), length = get16(&d[x + 2]);
            size_t f = x + 4;
            if (id == 1) {
                if (e.size == 0xffffffffu && f + 8 <= x + 4 + length)
                    e.size = get64(&d[f]), f += 8;
                if (e.compressed == 0xffffffffu && f + 8 <= x + 4 + length)
                    e.compressed = get64(&d[f]), f += 8;
                if (e.offset == 0xffffffffu && f + 8 <= x + 4 + length)
                    e.offset = get64(&d[f]), f += 8;
            }
            x += 4 + length;
        }
        uint8_t local[30];
        read_fully(fd, local, sizeof local, e.offset);
        require(get32(local) == local_signature, damaged, LT_IO);
        e.data = e.offset + 30 + get16(local + 26) + get16(local + 28);
        require(e.data + e.compressed <= directory, damaged, LT_IO);
        list.push_back(std::move(e));
        at += 46 + name + extra + comment;
    }
    if (const Entry *index = find(index_name)) {
        std::vector<uint8_t> bytes;
        require(index->size < (64u << 20), damaged, LT_IO);
        read(*index, [&](const uint8_t *p, size_t n) { bytes.insert(bytes.end(), p, p + n); });
        size_t p = 0;
        auto need = [&](size_t n) { require(p + n <= bytes.size(), damaged, LT_IO); };
        need(12);
        require(get32(&bytes[0]) == index_magic && get32(&bytes[4]) == 1, damaged, LT_IO);
        const uint32_t entries = get32(&bytes[8]);
        p = 12;
        for (uint32_t i = 0; i < entries; ++i) {
            need(2);
            const size_t length = get16(&bytes[p]);
            need(2 + length + 4);
            const std::string name(reinterpret_cast<const char *>(&bytes[p + 2]), length);
            const uint32_t chunks = get32(&bytes[p + 2 + length]);
            p += 2 + length + 4;
            need(size_t(chunks) * 4);
            auto it = std::find_if(list.begin(), list.end(), [&](const Entry &e) { return e.name == name; });
            require(it != list.end(), damaged, LT_IO);
            std::vector<uint32_t> sizes(chunks);
            uint64_t total = 0;
            for (uint32_t k = 0; k < chunks; ++k) {
                sizes[k] = get32(&bytes[p + size_t(k) * 4]);
                total += sizes[k] & ~zero_flag;
            }
            p += size_t(chunks) * 4;
            require(chunks == (it->size + chunk_bytes - 1) / chunk_bytes && total + 2 == it->compressed, damaged,
                    LT_IO);
            it->chunks = std::move(sizes);
        }
    }
}

const Entry *Reader::find(const std::string &name) const {
    for (const auto &e : list)
        if (e.name == name)
            return &e;
    return nullptr;
}

namespace {
// The entry's chunks inflated on every core, handed to sink in order (zero chunks flagged, their bytes still given).
void read_chunks(int fd, const Entry &e, const std::function<void(const uint8_t *, size_t, bool)> &sink,
                 const Progress &progress) {
    const size_t count = e.chunks.size();
    std::vector<uint64_t> offsets(count);
    for (size_t k = 0, at = 0; k < count; ++k) {
        offsets[k] = e.data + at;
        at += e.chunks[k] & ~zero_flag;
    }
    struct Slot {
        std::vector<uint8_t> data;
        uint32_t crc = 0;
        bool ready = false;
    };
    const unsigned threads = thread_count();
    const size_t window = threads * 3;
    std::vector<Slot> slots(window);
    std::mutex guard;
    std::condition_variable wake;
    size_t next = 0, consumed = 0;
    bool stop = false;
    std::exception_ptr failure;
    auto work = [&] {
        Inflater inflater;
        std::vector<uint8_t> in;
        for (;;) {
            size_t k;
            {
                std::unique_lock<std::mutex> lock(guard);
                wake.wait(lock, [&] { return stop || next >= count || next < consumed + window; });
                if (stop || next >= count)
                    return;
                k = next++;
            }
            Slot slot;
            try {
                const size_t length = size_t(std::min<uint64_t>(chunk_bytes, e.size - uint64_t(k) * chunk_bytes));
                slot.data.resize(length);
                if (e.chunks[k] & zero_flag) {
                    slot.crc = uint32_t(crc32(0, slot.data.data(), uInt(length)));
                } else {
                    in.resize(e.chunks[k]);
                    read_fully(fd, in.data(), in.size(), offsets[k]);
                    inflater.chunk(in.data(), in.size(), slot.data.data(), length);
                    slot.crc = uint32_t(crc32(0, slot.data.data(), uInt(length)));
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
                slot.ready = true;
                slots[k % window] = std::move(slot);
            }
            wake.notify_all();
        }
    };
    std::vector<std::thread> pool;
    for (unsigned t = 0; t < threads; ++t)
        pool.emplace_back(work);
    uint32_t crc = 0;
    try {
        for (size_t k = 0; k < count; ++k) {
            Slot slot;
            {
                std::unique_lock<std::mutex> lock(guard);
                wake.wait(lock, [&] { return failure || slots[k % window].ready; });
                if (failure)
                    std::rethrow_exception(failure);
                slot = std::move(slots[k % window]);
                slots[k % window] = Slot{};
                consumed = k + 1;
            }
            wake.notify_all();
            crc = uint32_t(crc32_combine(crc, slot.crc, static_cast<z_off_t>(slot.data.size())));
            sink(slot.data.data(), slot.data.size(), (e.chunks[k] & zero_flag) != 0);
            if (progress)
                progress(slot.data.size());
        }
        require(crc == e.crc, damaged, LT_IO);
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
}

// An entry with no chunk index (the index itself): one raw deflate stream.
void read_stream(int fd, const Entry &e, const std::function<void(const uint8_t *, size_t, bool)> &sink) {
    z_stream z{};
    require(inflateInit2(&z, -15) == Z_OK, "inflate setup", LT_IO);
    std::vector<uint8_t> in(1u << 16), out(chunk_bytes);
    uint64_t read = 0, produced = 0;
    uint32_t crc = 0;
    int r = Z_OK;
    try {
        while (r != Z_STREAM_END) {
            if (z.avail_in == 0) {
                require(read < e.compressed, damaged, LT_IO);
                const size_t n = size_t(std::min<uint64_t>(in.size(), e.compressed - read));
                read_fully(fd, in.data(), n, e.data + read);
                read += n;
                z.next_in = in.data();
                z.avail_in = uInt(n);
            }
            z.next_out = out.data();
            z.avail_out = uInt(out.size());
            r = inflate(&z, Z_NO_FLUSH);
            require(r == Z_OK || r == Z_STREAM_END, damaged, LT_IO);
            const size_t n = out.size() - z.avail_out;
            crc = uint32_t(crc32(crc, out.data(), uInt(n)));
            produced += n;
            require(produced <= e.size, damaged, LT_IO);
            if (n)
                sink(out.data(), n, false);
        }
        require(produced == e.size && crc == e.crc, damaged, LT_IO);
    } catch (...) {
        inflateEnd(&z);
        throw;
    }
    inflateEnd(&z);
}
} // namespace

void Reader::read(const Entry &e, const std::function<void(const uint8_t *, size_t)> &sink, Progress progress) const {
    auto adapt = [&](const uint8_t *p, size_t n, bool) { sink(p, n); };
    if (e.chunks.empty() && e.size)
        read_stream(fd, e, adapt);
    else
        read_chunks(fd, e, adapt, progress);
}

void Reader::extract(const Entry &e, const fs::path &path, Progress progress) const {
    Fd out(lt::os::open(path.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600));
    io(out.n >= 0, "create " + path.string());
    uint64_t at = 0;
    auto sink = [&](const uint8_t *p, size_t n, bool zero) {
        if (!zero) {
            for (size_t done = 0; done < n;) {
                ssize_t k = lt::os::pwrite(out.n, p + done, n - done, int64_t(at + done));
                if (k < 0 && errno == EINTR)
                    continue;
                if (k < 0 && errno == ENOSPC)
                    throw Failure(LT_IO, "There isn't enough free space to load the state");
                io(k > 0, "write " + path.string());
                done += size_t(k);
            }
        }
        at += n;
    };
    if (e.chunks.empty() && e.size)
        read_stream(fd, e, sink);
    else
        read_chunks(fd, e, sink, progress);
    io(lt::os::ftruncate(out.n, int64_t(e.size)) == 0, "size " + path.string());
    io(lt::os::fsync(out.n) == 0, "sync " + path.string());
}

} // namespace lt::zip
