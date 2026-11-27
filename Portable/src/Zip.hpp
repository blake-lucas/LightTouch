#pragma once
// A ZIP64 writer and reader that deflate and inflate on every core, for save states (States.cpp).
//
// Entries are cut into 1 MiB chunks, each deflated on its own (level 1, sync-flushed, so the chunks concatenate into
// one raw deflate stream any unzip tool reads), as device transfer does (Transfer.cpp). An entry's bytes may arrive as
// a stream (the engine's machine state) or come from a file, whose holes are never read. Output stays in order however
// the chunks finish. Each entry's chunk sizes go into a last entry, "chunks.bin", so the reader inflates an entry's
// chunks in parallel too, and writes all-zero chunks as holes.
#include "Internal.hpp"
#include <condition_variable>
#include <deque>
#include <functional>
#include <mutex>
#include <thread>

namespace lt::zip {

constexpr size_t chunk_bytes = 1u << 20;

// Bytes that went in (uncompressed), for progress.
using Progress = std::function<void(uint64_t)>;

class Writer {
  public:
    // fd stays the caller's. Every core's worth of threads, at most 8.
    Writer(int fd, Progress progress);
    ~Writer();
    Writer(const Writer &) = delete;
    Writer &operator=(const Writer &) = delete;

    void begin(const std::string &name);
    void write(const void *data, size_t size);
    void end();
    // A whole entry from a file, its holes as zero chunks. Throws if it shrinks meanwhile.
    void file(const std::string &name, const fs::path &path);
    // The chunk index and the central directory; the fd then holds a complete zip.
    void finish();
    uint64_t written() const { return out_offset; }

  private:
    struct Item;
    struct Pending;
    void submit(Item item);
    void flush_chunk();
    void work();
    void output();
    void fail(std::exception_ptr);
    void rethrow();

    int fd;
    Progress progress;
    std::vector<std::thread> pool;
    std::thread writer;
    std::mutex guard;
    std::condition_variable wake;
    std::deque<std::shared_ptr<Pending>> queue; // in output order
    size_t window = 1;
    bool closing = false;
    std::exception_ptr failure;
    std::vector<uint8_t> chunk; // the stream entry's chunk being filled
    bool in_entry = false;
    uint64_t out_offset = 0;
    uint16_t time = 0, date = 0;
    struct Central;
    std::vector<Central> *central;
};

struct Entry {
    std::string name;
    uint64_t offset = 0, data = 0, compressed = 0, size = 0; // local header, data start
    uint32_t crc = 0;
    std::vector<uint32_t> chunks; // compressed size of each chunk (from chunks.bin); empty: inflate as one stream
};

class Reader {
  public:
    // fd stays the caller's; the zip must be one this Writer made.
    explicit Reader(int fd);
    const Entry *find(const std::string &name) const;
    const std::vector<Entry> &entries() const { return list; }
    // An entry's bytes, in order, to sink (which may throw to stop). Inflated on every core, checked against its CRC.
    void read(const Entry &, const std::function<void(const uint8_t *, size_t)> &sink, Progress progress = {}) const;
    // An entry into a new file (O_EXCL), all-zero chunks left as holes, synced.
    void extract(const Entry &, const fs::path &path, Progress progress = {}) const;

  private:
    int fd;
    std::vector<Entry> list;
};

} // namespace lt::zip
