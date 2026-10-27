// The NAND container: the writer's bytes (the same as FirmwareKit's nand_disk.rs for the same puts), and imports that
// turn a store of chip files or the 2G's page files into a record's nand/nand.qcow2, from a folder or a streamed
// archive, with geometry.json before or after the chips: all write the same container.
#include "lighttouch.h"
#include "../src/NandDisk.hpp"
#include <zlib.h>
#include <cassert>
#include <fstream>
#include <iostream>
#include <unistd.h>
namespace fs = std::filesystem;
static void put(const fs::path &p, const std::string &s) {
    fs::create_directories(p.parent_path());
    std::ofstream f(p, std::ios::binary);
    f.write(s.data(), std::streamsize(s.size()));
    assert(f.good());
}
static std::string read(const fs::path &p) {
    std::ifstream f(p, std::ios::binary);
    return std::string(std::istreambuf_iterator<char>(f), {});
}
static std::string sha(const std::string &s) {
    lt::Sha256 h;
    h.update(s.data(), s.size());
    return h.finish();
}
static uint64_t be64(const std::string &b, uint64_t at) {
    uint64_t v = 0;
    for (int i = 0; i < 8; ++i)
        v = v << 8 | uint8_t(b[size_t(at) + i]);
    return v;
}
// Guest bytes of a container, by walking its own tables as a qcow2 reader does.
static std::string guest(const std::string &image, uint64_t at, size_t n) {
    using namespace lt::nand_disk;
    std::string out;
    const uint64_t l1 = be64(image, 40);
    for (uint64_t g = at; g < at + n; ++g) {
        const uint64_t l2 = be64(image, l1 + g / cluster / l2_entries * 8) & ~copied;
        const uint64_t host = l2 ? be64(image, l2 + g / cluster % l2_entries * 8) & ~copied : 0;
        out += host ? image[size_t(host + g % cluster)] : '\0';
    }
    return out;
}
static void archive(const fs::path &path, const std::vector<std::pair<std::string, std::string>> &entries) {
    std::string index = "{\"entries\":[", data;
    for (size_t i = 0; i < entries.size(); i++) {
        index += (i ? "," : "") + std::string("{\"name\":\"") + entries[i].first + "\",\"size\":" +
                 std::to_string(entries[i].second.size()) + ",\"mode\":420}";
        data += entries[i].second;
    }
    index += "]}";
    std::string blob = "ITPACK01";
    for (int i = 0; i < 4; i++)
        blob += char(uint32_t(index.size()) >> (i * 8));
    uLongf cap = compressBound(uLong(data.size()));
    std::string stream(cap, '\0');
    assert(compress2(reinterpret_cast<Bytef *>(stream.data()), &cap, reinterpret_cast<const Bytef *>(data.data()),
                     uLong(data.size()), 6) == Z_OK);
    stream.resize(cap);
    put(path, blob + index + stream);
}
int main() {
    using lt::nand_disk::cluster;
    using lt::nand_disk::l2_entries;
    char name[] = "/tmp/lighttouch-nand-disk-XXXXXX";
    assert(mkdtemp(name));
    const fs::path root = name;
    lt_error e{sizeof(e), 1, 0, {}};
    {
        // nand_disk.rs writer_matches_the_core: the same puts, the same bytes.
        lt::nand_disk::Writer w(root / "golden.qcow2", 2 * cluster * l2_entries);
        std::vector<uint8_t> a(70000);
        for (uint32_t i = 0; i < a.size(); ++i)
            a[i] = uint8_t(i * 7 % 256);
        w.put(cluster * l2_entries + 4000, a.data(), a.size());
        w.put(3, reinterpret_cast<const uint8_t *>("LightTouch"), 10);
        w.finish();
        assert(sha(read(root / "golden.qcow2")) == "49fd8076235f5fe2c6c40391b2b7d61c9bdab4f8181d4a93987d9b0422ef1364");
    }
    // A small A4-shaped store: 1 bus x 2 CEs of 4 blocks of 8 records of 4096 + 128 bytes (block 33792 B, so a
    // 64 KiB block stride), with data in block 0 and across the end of block 2 into block 3.
    const std::string geometry = "{\n \"page_bytes\": 4096,\n \"spare_bytes\": 128,\n \"pages_per_block\": 8,\n"
                                 " \"blocks_per_ce\": 4,\n \"ce_per_bus\": 2,\n \"buses\": 1,\n \"chip_id\": \"0xB614D5AD\","
                                 "\n \"storage_format\": \"nand-xor-ff-v2\"\n}";
    const uint64_t block = 8 * 4224, chip_bytes = 4 * block;
    std::string data(10000, '\0');
    for (size_t i = 0; i < data.size(); ++i)
        data[i] = char(1 + i % 250);
    std::vector<std::string> chips(2, std::string(chip_bytes, '\0'));
    chips[0].replace(100, 5000, data, 0, 5000);
    chips[1].replace(3 * block - 3000, 6000, data, 0, 6000);
    std::string listing = "bus0-ce0.pages " + sha(chips[0]) + "\nbus0-ce1.pages " + sha(chips[1]) +
                          "\ngeometry.json " + sha(geometry) + "\n";
    const std::string kboot = "kboot";
    const std::string lock = "{\"format\":1,\"board\":\"n81ap\",\"build\":\"10B500\",\"boot_strategy\":\"kboot\",\"outputs\":{"
                             "\"kboot\":{\"path\":\"kboot.bin\",\"sha256\":\"" + sha(kboot) + "\"},\"nand\":{\"path\":\"nand\","
                             "\"listing_sha256\":\"" + sha(listing) + "\"}}}";
    auto check = [&](const fs::path &base) {
        std::vector<std::string> names;
        for (const auto &entry : fs::directory_iterator(base / "nand"))
            names.push_back(entry.path().filename().string());
        std::sort(names.begin(), names.end());
        assert((names == std::vector<std::string>{"geometry.json", "nand.qcow2"}));
        const auto g = lt::parse(read(base / "nand/geometry.json"));
        assert(g.at("container").string() == "qcow2" && g.at("block_stride").integer() == cluster &&
               g.at("chip_stride").integer() == 4 * cluster && g.at("chip_id").string() == "0xB614D5AD");
        const auto lock = lt::parse(read(base / "device.lock.json"));
        const auto &nand = lock.at("outputs").at("nand");
        assert(nand.at("source_listing_sha256").string() == sha(listing));
        const std::string image = read(base / "nand/nand.qcow2");
        assert(nand.at("files").at("nand.qcow2").string() == sha(image));
        assert(fs::exists(base / "source-device.lock.json"));
        assert(guest(image, 100, 5000) == data.substr(0, 5000));
        assert(guest(image, 4 * cluster + 2 * cluster + block - 3000, 3000) == data.substr(0, 3000));
        assert(guest(image, 4 * cluster + 3 * cluster, 3000) == data.substr(3000, 3000));
        assert(guest(image, 4 * cluster + 3 * cluster - 64, 64) == std::string(64, '\0'));  // block padding
        return image;
    };
    // From a folder: chip files read for their data, never copied.
    put(root / "folder/device.lock.json", lock);
    put(root / "folder/kboot.bin", kboot);
    put(root / "folder/nand/geometry.json", geometry);
    put(root / "folder/nand/bus0-ce0.pages", chips[0]);
    put(root / "folder/nand/bus0-ce1.pages", chips[1]);
    const std::string folder = (root / "folder").string(), base = (root / "base").string();
    lt_import_config config{sizeof(config), 1, folder.c_str(), base.c_str(), nullptr, 0, nullptr, nullptr};
    if (lt_import_directory(&config, &e) != LT_OK)
        std::cerr << e.message << "\n";
    assert(e.code == LT_OK);
    const std::string container = check(base);
    // Admitted as any base, and booted with its overlay in state/nand.
    const std::string state = (root / "state").string();
    lt_device_config dc{sizeof(dc), 1, base.c_str(), state.c_str(), 0};
    lt_device *d = nullptr;
    assert(lt_device_open(&dc, &d, &e) == LT_OK);
    lt_boot_config boot{sizeof(boot), 1, "n81", nullptr, nullptr, nullptr, 0};
    int argc;
    const char *const *argv;
    assert(lt_device_boot_argv(d, &boot, &argc, &argv, &e) == LT_OK);
    const std::string args = argv[2];
    assert(args.find("nand=" + fs::canonical(base).string() + "/nand") != std::string::npos &&
           args.find("nand-overlay=" + fs::canonical(state).string() + "/nand") != std::string::npos);
    lt_device_close(d);
    // A tampered chip fails the source listing.
    put(root / "bad/device.lock.json", lock);
    put(root / "bad/kboot.bin", kboot);
    put(root / "bad/nand/geometry.json", geometry);
    put(root / "bad/nand/bus0-ce0.pages", chips[1]);
    put(root / "bad/nand/bus0-ce1.pages", chips[1]);
    const std::string bad = (root / "bad").string(), bad_base = (root / "bad-base").string();
    config.source = bad.c_str();
    config.destination = bad_base.c_str();
    assert(lt_import_directory(&config, &e) == LT_INVALID && !fs::exists(bad_base));
    // From an archive, geometry.json first (streamed into the container) or last (chips staged, then packed): the
    // same container either way.
    for (bool geometry_first : {true, false}) {
        std::vector<std::pair<std::string, std::string>> entries{{"device.lock.json", lock}, {"kboot.bin", kboot}};
        if (geometry_first)
            entries.emplace_back("nand/geometry.json", geometry);
        entries.emplace_back("nand/bus0-ce0.pages", chips[0]);
        entries.emplace_back("nand/bus0-ce1.pages", chips[1]);
        if (!geometry_first)
            entries.emplace_back("nand/geometry.json", geometry);
        archive(root / "base.itbase", entries);
        const std::string source = (root / "base.itbase").string(), expected = sha(read(root / "base.itbase"));
        const std::string out = (root / (geometry_first ? "archive-first" : "archive-last")).string();
        lt_import_config a{sizeof(a), 1, source.c_str(), out.c_str(), expected.c_str(), 0, nullptr, nullptr};
        if (lt_import_archive(&a, &e) != LT_OK)
            std::cerr << e.message << "\n";
        assert(e.code == LT_OK);
        assert(check(out) == container);
    }
    // The 2G's page files (nand/csN/N.page, as the Swift tools wrote them) become its container too: each record
    // stored XOR the n72 blank, at its place in the padded layout.
    {
        std::string page(4160, '\0');
        for (size_t i = 0; i < page.size(); ++i)
            page[i] = char(i % 199 + 1);
        const std::string listing = "cs1/129.page " + sha(page) + "\n";
        const std::string iboot = "iboot", nor = "nor", gid = "gid";
        const std::string lock = "{\"format\":1,\"board\":\"n72ap\",\"build\":\"7E18\",\"boot_strategy\":\"iboot\",\"outputs\":{"
                                 "\"iboot\":{\"path\":\"iBoot.bin\",\"sha256\":\"" + sha(iboot) + "\"},\"nor\":{\"path\":"
                                 "\"nor.bin\",\"sha256\":\"" + sha(nor) + "\"},\"gid_blobs\":{\"path\":\"gid-blobs.bin\","
                                 "\"sha256\":\"" + sha(gid) + "\"},\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" +
                                 sha(listing) + "\",\"pages\":1}}}";
        archive(root / "n72.itbase", {{"device.lock.json", lock}, {"gid-blobs.bin", gid}, {"iBoot.bin", iboot},
                                      {"nand/cs1/129.page", page}, {"nor.bin", nor}});
        const std::string source = (root / "n72.itbase").string(), expected = sha(read(root / "n72.itbase"));
        const std::string out = (root / "n72").string();
        lt_import_config a{sizeof(a), 1, source.c_str(), out.c_str(), expected.c_str(), 0, nullptr, nullptr};
        if (lt_import_archive(&a, &e) != LT_OK)
            std::cerr << e.message << "\n";
        assert(e.code == LT_OK);
        const auto layout = lt::nand_disk::Layout::make(4, 4160, 128, 4096);
        assert(layout.block_stride == 9 * cluster);
        std::string stored = page;
        stored[4096 + 8] ^= char(0xff);
        stored[4096 + 10] ^= char(0xff);
        const std::string image = read(fs::path(out) / "nand/nand.qcow2");
        assert(guest(image, layout.place(1, 129 * 4160ull), 4160) == stored);
        assert(lt::parse(read(fs::path(out) / "nand/geometry.json")).at("blank").string() == "n72-generated");
        // Booted with its overlay: no live copy of the base's NAND.
        const std::string n72state = (root / "n72state").string();
        lt_device_config dc{sizeof(dc), 1, out.c_str(), n72state.c_str(), 0};
        assert(lt_device_open(&dc, &d, &e) == LT_OK);
        put(root / "rom", "rom");
        const std::string rom = (root / "rom").string();
        lt_boot_config boot{sizeof(boot), 1, "iPod-Touch-2G", rom.c_str(), nullptr, nullptr, 0};
        assert(lt_device_boot_argv(d, &boot, &argc, &argv, &e) == LT_OK);
        const std::string args = argv[2];
        assert(args.find("nandrw=" + fs::canonical(n72state).string() + "/nand") != std::string::npos &&
               args.find("nand=" + fs::canonical(out).string() + "/nand") != std::string::npos);
        assert(!fs::exists(fs::path(n72state) / "nand/geometry.json"));
        lt_device_close(d);
    }
    fs::remove_all(root);
    std::cout << "nand disk: ok\n";
}
