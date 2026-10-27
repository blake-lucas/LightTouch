#pragma once
// A device's NAND as one qcow2 disk (qemu-ios hw/arm/nand-image.h, "NAND container"): every chip in nand.qcow2,
// chip c's byte `at` (its chip file's offset) at c * chip_stride + (at / block_bytes) * block_stride +
// at % block_bytes, so an erase block starts on a 64 KiB cluster. This is the system image, written once when a
// store is imported; the engine writes the device's own overlay. The writer is FirmwareKit's
// (crates/lighttouch-firmware/src/nand_disk.rs): for the same input both write the same bytes.
#include "Internal.hpp"
#include "Os.hpp"
#include <algorithm>
#include <map>
#include <mutex>
namespace lt::nand_disk {
constexpr const char *file_name = "nand.qcow2";
constexpr uint64_t cluster = 1ull << 16, l2_entries = cluster / 8, refcounts_per_block = cluster / 2,
                   copied = 1ull << 63;
constexpr size_t sparse_block = 4096;

struct Layout {
    uint64_t chips = 0, stride = 0, pages_per_block = 0, blocks_per_chip = 0, block_stride = 0, chip_stride = 0;
    std::vector<std::string> names;  // the chip files, in chip order
    static Layout make(uint64_t chips, uint64_t stride, uint64_t ppb, uint64_t blocks) {
        Layout l;
        l.chips = chips;
        l.stride = stride;
        l.pages_per_block = ppb;
        l.blocks_per_chip = blocks;
        l.block_stride = (ppb * stride + cluster - 1) / cluster * cluster;
        l.chip_stride = blocks * l.block_stride;
        return l;
    }
    // An A4 board's store (buses x ce_per_bus, bus<b>-ce<c>.pages) or a page-directory board's (chip_selects,
    // cs<N>.pages), as geometry.json describes it.
    static Layout of(const Json &g) {
        auto number = [&](const char *key) -> uint64_t {
            const Json *v = g.find(key);
            const uint64_t x = v && v->kind == Json::Number ? v->integer() : 0;
            require(x > 0 && x < (1ull << 40), std::string("geometry.json: invalid ") + key);
            return x;
        };
        std::vector<std::string> names;
        if (g.find("buses") && g.find("ce_per_bus")) {
            const uint64_t buses = number("buses"), per_bus = number("ce_per_bus");
            require(buses * per_bus <= 64, "geometry.json: too many chips");
            for (uint64_t k = 0; k < buses * per_bus; ++k)
                names.push_back("bus" + std::to_string(k / per_bus) + "-ce" + std::to_string(k % per_bus) + ".pages");
        } else {
            const uint64_t cs = number("chip_selects");
            require(cs <= 64, "geometry.json: too many chips");
            for (uint64_t k = 0; k < cs; ++k)
                names.push_back("cs" + std::to_string(k) + ".pages");
        }
        Layout l = make(names.size(), number("page_bytes") + number("spare_bytes"), number("pages_per_block"),
                        number("blocks_per_ce"));
        l.names = std::move(names);
        return l;
    }
    // Whether geometry.json's text describes a store of chip files.
    static bool known(const std::string &text) {
        try {
            return !of(parse(text)).names.empty();
        } catch (const std::exception &) {
            return false;
        }
    }
    uint64_t block_bytes() const { return pages_per_block * stride; }
    uint64_t chip_bytes() const { return blocks_per_chip * block_bytes(); }
    uint64_t size() const { return chips * chip_stride; }
    uint64_t place(uint64_t chip, uint64_t at) const {
        return chip * chip_stride + at / block_bytes() * block_stride + at % block_bytes();
    }
    // geometry.json's text with the container's keys added before its closing brace.
    std::string geometry(std::string text) const {
        while (!text.empty() && isspace(static_cast<unsigned char>(text.back())))
            text.pop_back();
        require(!text.empty() && text.back() == '}', "geometry.json is not an object");
        text.pop_back();
        while (!text.empty() && isspace(static_cast<unsigned char>(text.back())))
            text.pop_back();
        return text + ",\n \"block_stride\": " + std::to_string(block_stride) + ",\n \"chip_stride\": " +
               std::to_string(chip_stride) + ",\n \"container\": \"qcow2\"\n}";
    }
};

// The iPod touch 2G's store (n72-generated layout; page_nand.rs Image::n72): geometry.json as FirmwareKit writes it,
// and its blank record, the XOR key of every stored record (zero data, spare bytes 8 and 10 0xFF).
inline const std::string &n72_geometry() {
    static const std::string text = "{\n  \"blank\" : \"n72-generated\",\n  \"blocks_per_ce\" : 4096,\n  \"board\" : "
                                    "\"n72ap\",\n  \"chip_selects\" : 4,\n  \"layout\" : \"n72-generated-v1\",\n  "
                                    "\"page_bytes\" : 4096,\n  \"pages_per_block\" : 128,\n  \"spare_bytes\" : 64,\n  "
                                    "\"storage_format\" : \"nand-xor-blank-v2\"\n}";
    return text;
}
constexpr uint32_t n72_record = 4160;
inline void n72_encode(uint8_t *record) {
    record[4096 + 8] ^= 0xff;
    record[4096 + 10] ^= 0xff;
}

inline bool zero(const uint8_t *p, size_t n) { return std::all_of(p, p + n, [](uint8_t b) { return b == 0; }); }

// A qcow2 image (version 3, 64 KiB clusters, 16-bit refcounts, no backing file) written once: data clusters in the
// order they are first given, then the L2 tables, L1 table, refcount table and refcount blocks, then the header.
// Only clusters holding a nonzero byte are stored, and only their nonzero 4 KiB blocks written. put() may be called
// from several threads.
class Writer {
  public:
    Writer(const fs::path &path, uint64_t size) : path_(path), size_(size) {
        fd_ = lt::os::open(path.c_str(), O_RDWR | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
        io(fd_ >= 0, "create " + path.string());
    }
    ~Writer() {
        if (fd_ >= 0)
            lt::os::close(fd_);
    }
    Writer(const Writer &) = delete;
    // `n` bytes at guest offset `at`; each guest byte is given at most once (zeros need not be given at all).
    void put(uint64_t at, const uint8_t *data, size_t n) {
        require(at <= size_ && n <= size_ - at, "write past the end of the NAND disk");
        while (n) {
            const uint64_t within = at % cluster;
            const size_t take = size_t(std::min<uint64_t>(n, cluster - within));
            if (!zero(data, take)) {
                uint64_t host;
                {
                    std::lock_guard<std::mutex> lock(guard_);
                    auto [it, fresh] = map_.emplace(at / cluster, next_);
                    if (fresh)
                        next_ += cluster;
                    host = it->second;
                }
                write_sparse(host + within, data, take);
            }
            at += take;
            data += take;
            n -= take;
        }
    }
    // Writes the metadata and header and syncs the file.
    void finish() {
        const uint64_t l1_size = (size_ + cluster * l2_entries - 1) / (cluster * l2_entries);
        std::vector<uint64_t> tables;
        for (const auto &entry : map_)
            if (tables.empty() || tables.back() != entry.first / l2_entries)
                tables.push_back(entry.first / l2_entries);
        const uint64_t l1_clusters = std::max<uint64_t>(1, (l1_size * 8 + cluster - 1) / cluster);
        const uint64_t fixed = next_ / cluster + tables.size() + l1_clusters;
        uint64_t table_clusters = 1, blocks = 1;
        for (;;) {
            const uint64_t total = fixed + table_clusters + blocks;
            const uint64_t b = (total + refcounts_per_block - 1) / refcounts_per_block, t = (b * 8 + cluster - 1) / cluster;
            if (b == blocks && t == table_clusters)
                break;
            blocks = b;
            table_clusters = t;
        }
        const uint64_t total = fixed + table_clusters + blocks;
        uint64_t at = next_;
        std::vector<uint8_t> l1(static_cast<size_t>(l1_clusters * cluster), 0), buffer(static_cast<size_t>(cluster), 0);
        auto entries = map_.begin();
        for (uint64_t t : tables) {
            std::fill(buffer.begin(), buffer.end(), 0);
            for (; entries != map_.end() && entries->first / l2_entries == t; ++entries)
                be64(buffer.data() + (entries->first % l2_entries) * 8, entries->second | copied);
            write_all(at, buffer.data(), buffer.size());
            be64(l1.data() + t * 8, at | copied);
            at += cluster;
        }
        const uint64_t l1_at = at;
        write_all(l1_at, l1.data(), l1.size());
        at += l1_clusters * cluster;
        const uint64_t table_at = at;
        std::vector<uint8_t> table(size_t(table_clusters * cluster), 0);
        for (uint64_t b = 0; b < blocks; ++b)
            be64(table.data() + b * 8, table_at + (table_clusters + b) * cluster);
        write_all(table_at, table.data(), table.size());
        at += table_clusters * cluster;
        for (uint64_t b = 0; b < blocks; ++b) {
            std::fill(buffer.begin(), buffer.end(), 0);
            const uint64_t first = b * refcounts_per_block;
            for (uint64_t c = first; c < std::min(total, first + refcounts_per_block); ++c)
                buffer[size_t((c - first) * 2 + 1)] = 1;
            write_all(at, buffer.data(), buffer.size());
            at += cluster;
        }
        uint8_t h[112] = {};
        be32(h + 0, 0x514649fb);  // "QFI\xfb"
        be32(h + 4, 3);           // version
        be32(h + 20, 16);         // cluster_bits
        be64(h + 24, size_);
        be32(h + 36, uint32_t(l1_size));
        be64(h + 40, l1_at);
        be64(h + 48, table_at);
        be32(h + 56, uint32_t(table_clusters));
        be32(h + 96, 4);    // refcount_order: 16-bit
        be32(h + 100, 104); // header_length; 104..111: the end of the (empty) extension list
        write_all(0, h, sizeof h);
        io(lt::os::fsync(fd_) == 0, "sync " + path_.string());
    }

  private:
    static void be64(uint8_t *p, uint64_t v) {
        for (int i = 0; i < 8; ++i)
            p[i] = uint8_t(v >> (56 - 8 * i));
    }
    static void be32(uint8_t *p, uint32_t v) {
        for (int i = 0; i < 4; ++i)
            p[i] = uint8_t(v >> (24 - 8 * i));
    }
    void write_all(uint64_t at, const uint8_t *p, size_t n) {
        while (n) {
            ssize_t k = lt::os::pwrite(fd_, p, n, int64_t(at));
            if (k < 0 && errno == EINTR)
                continue;
            io(k > 0, "write " + path_.string());
            p += k;
            at += uint64_t(k);
            n -= size_t(k);
        }
    }
    // `p` at host offset `at`, leaving out its all-zero host blocks.
    void write_sparse(uint64_t at, const uint8_t *p, size_t n) {
        size_t i = 0;
        auto end = [&](size_t i) { return std::min(n, i + sparse_block - size_t((at + i) % sparse_block)); };
        while (i < n) {
            if (zero(p + i, end(i) - i)) {
                i = end(i);
                continue;
            }
            const size_t start = i;
            while (i < n && !zero(p + i, end(i) - i))
                i = end(i);
            write_all(at + start, p + start, i - start);
        }
    }
    fs::path path_;
    uint64_t size_;
    int fd_ = -1;
    std::mutex guard_;
    std::map<uint64_t, uint64_t> map_;  // guest cluster -> host offset
    uint64_t next_ = cluster;
};
}  // namespace lt::nand_disk
