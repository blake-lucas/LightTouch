#include "lighttouch.h"
#include "../src/Sha256.hpp"
#include <zlib.h>
#include <algorithm>
#include <cassert>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <map>
#include <string>
#include <sys/stat.h>
#include <unistd.h>
#include <vector>
namespace fs = std::filesystem;
static std::string sha(const std::string &bytes) {
    lt::Sha256 value;
    value.update(bytes.data(), bytes.size());
    return value.finish();
}
static void put(const fs::path &path, const std::string &bytes) {
    std::ofstream out(path, std::ios::binary);
    out.write(bytes.data(), bytes.size());
    assert(out.good());
}
static std::string read(const fs::path &path) {
    std::ifstream in(path, std::ios::binary);
    assert(in.good());
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}
static void archive(const fs::path &path, const std::vector<std::pair<std::string, std::string>> &entries,
                    const std::string &extra_payload = "") {
    std::string index = "{\"entries\":[", payload;
    for (size_t n = 0; n < entries.size(); ++n) {
        if (n)
            index += ",";
        index += "{\"name\":\"" + entries[n].first + "\",\"size\":" +
                 std::to_string(entries[n].second.size()) + ",\"mode\":420}";
        payload += entries[n].second;
    }
    index += "]}";
    payload += extra_payload;
    std::string bytes = "ITPACK01";
    for (int n = 0; n < 4; ++n)
        bytes += char(uint32_t(index.size()) >> (n * 8));
    bytes += index;
    uLongf capacity = compressBound(payload.size());
    std::string compressed(capacity, '\0');
    assert(compress2(reinterpret_cast<Bytef *>(compressed.data()), &capacity,
                     reinterpret_cast<const Bytef *>(payload.data()), payload.size(), 1) == Z_OK);
    compressed.resize(capacity);
    put(path, bytes + compressed);
}
static void discard(const fs::path &root) {
    for (const auto &entry : fs::recursive_directory_iterator(root))
        if (entry.is_directory())
            fs::permissions(entry.path(), fs::perms::owner_all, fs::perm_options::add);
    fs::permissions(root, fs::perms::owner_all, fs::perm_options::add);
    fs::remove_all(root);
}
struct Observation {
    std::string cancel;
    bool after_progress = false;
    std::vector<std::string> phases;
    static int notify(void *opaque, const char *phase, uint64_t done, uint64_t total) {
        auto &self = *static_cast<Observation *>(opaque);
        assert(done <= total);
        if (self.phases.empty() || self.phases.back() != phase)
            self.phases.emplace_back(phase);
        return self.cancel == phase && (!self.after_progress || done > 0);
    }
};
int main() {
    char name[] = "/tmp/lighttouch-import-sparse-XXXXXX";
    assert(mkdtemp(name));
    const fs::path root(name);
    std::map<std::string, std::string> nand;
    nand["allzero"] = std::string(8 * 1024 * 1024 + 7, '\0');
    nand["empty"] = "";
    auto &mixed = nand["mixed"];
    mixed.assign(2 * 65536 + 137, '\0');
    for (size_t offset : {size_t(0), size_t(65535), size_t(65536), size_t(131072), mixed.size() - 1})
        mixed[offset] = char(1 + offset % 254);
    nand["trailing"] = "prefix" + std::string(2 * 65536 + 17, '\0');
    std::string listing;
    for (const auto &file : nand)
        listing += file.first + " " + sha(file.second) + "\n";
    const std::string nor(65537, '\0'), boot = "boot", gid = "gid";
    std::string lock = "{\"format\":1,\"board\":\"n72ap\",\"build\":\"synthetic\",\"outputs\":{"
                       "\"nor\":{\"path\":\"nor.bin\",\"sha256\":\"" + sha(nor) +
                       "\"},\"iboot\":{\"path\":\"iBoot.bin\",\"sha256\":\"" + sha(boot) +
                       "\"},\"gid_blobs\":{\"path\":\"gid-blobs.bin\",\"sha256\":\"" + sha(gid) +
                       "\"},\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" + sha(listing) +
                       "\",\"pages\":4}}}";
    std::vector<std::pair<std::string, std::string>> entries{{"device.lock.json", lock}, {"nor.bin", nor},
                                                            {"iBoot.bin", boot}, {"gid-blobs.bin", gid},
                                                            {"zero-length-first", ""}};
    for (const auto &file : nand)
        entries.emplace_back("nand/" + file.first, file.second);
    entries.emplace_back("zero-length-last", "");
    fs::path input = root / "fixture.itbase";
    archive(input, entries);
    std::string source = input.string(), destination = (root / "base").string(), expected = sha(read(input));
    lt_import_config config{sizeof(config), 1, source.c_str(), destination.c_str(), expected.c_str(), 0, nullptr, nullptr};
    lt_error error{sizeof(error), 1, 0, {}};
    // A caller-written stage publishes by rename, with no recopy and no change
    // to its modes. Failed or cancelled finalization leaves it for the caller.
    auto make_stage = [&] {
        const auto stage = root / ".lt-import-caller";
        fs::create_directory(stage);
        fs::permissions(stage, fs::perms::owner_all);
        for (const auto &file : entries) {
            fs::create_directories((stage / file.first).parent_path());
            put(stage / file.first, file.second);
            fs::permissions(stage / file.first, fs::perms::owner_read | fs::perms::owner_write);
        }
        fs::permissions(stage / "nand", fs::perms::owner_all);
        return stage;
    };
    std::string stage_path = make_stage().string();
    lt_finalize_import_config finalize{sizeof(finalize), 1, stage_path.c_str(), destination.c_str()};
    struct stat staged_inode {}, published_inode {};
    assert(stat((fs::path(stage_path) / "nand/allzero").c_str(), &staged_inode) == 0);
    Observation finalized;
    const auto finalized_status = lt_finalize_import_observed(&finalize, Observation::notify, &finalized, &error);
    if (finalized_status != LT_OK)
        std::cerr << error.message << "\n";
    assert(finalized_status == LT_OK);
    assert((finalized.phases == std::vector<std::string>{"verifying"}));
    assert(!fs::exists(stage_path) && fs::exists(destination));
    assert(stat((fs::path(destination) / "nand/allzero").c_str(), &published_inode) == 0);
    assert(staged_inode.st_dev == published_inode.st_dev && staged_inode.st_ino == published_inode.st_ino);
    assert((fs::status(destination).permissions() & fs::perms::owner_write) != fs::perms::none);
    assert((fs::status(fs::path(destination) / "nand/allzero").permissions() & fs::perms::owner_write) !=
           fs::perms::none);
    discard(destination);
    {
        make_stage();
        Observation cancel;
        cancel.cancel = "verifying";
        cancel.after_progress = true;
        assert(lt_finalize_import_observed(&finalize, Observation::notify, &cancel, &error) == LT_CANCELLED);
        assert(!fs::exists(destination) && read(fs::path(stage_path) / "nand/allzero") == nand.at("allzero"));
        discard(stage_path);
    }
    make_stage();
    fs::create_directory(destination);
    put(fs::path(destination) / "keep", "existing destination");
    assert(lt_finalize_import(&finalize, &error) == LT_INVALID);
    assert(read(fs::path(destination) / "keep") == "existing destination" && fs::exists(stage_path));
    discard(destination);
    discard(stage_path);
    make_stage();
    put(fs::path(stage_path) / "nand/mixed", "tampered");
    assert(lt_finalize_import(&finalize, &error) == LT_INVALID);
    assert(fs::exists(stage_path) && !fs::exists(destination));
    discard(stage_path);
    for (bool hardlink : {false, true}) {
        make_stage();
        const auto external = root / "outside";
        put(external, "external data");
        if (hardlink)
            fs::create_hard_link(external, fs::path(stage_path) / "alias");
        else
            fs::create_symlink(external, fs::path(stage_path) / "alias");
        assert(lt_finalize_import(&finalize, &error) == LT_INVALID);
        assert((fs::status(external).permissions() & fs::perms::owner_write) != fs::perms::none);
        assert(fs::exists(stage_path) && !fs::exists(destination));
        discard(stage_path);
        fs::remove(external);
    }
    make_stage();
    fs::create_directory(root / "other");
    std::string other_destination = (root / "other/base").string();
    finalize.destination = other_destination.c_str();
    assert(lt_finalize_import(&finalize, &error) == LT_INVALID && fs::exists(stage_path));
    finalize.destination = destination.c_str();
    fs::permissions(stage_path, fs::perms::group_read, fs::perm_options::add);
    assert(lt_finalize_import(&finalize, &error) == LT_INVALID && fs::exists(stage_path));
    discard(stage_path);
    Observation observed;
    assert(lt_import_archive_observed(&config, Observation::notify, &observed, &error) == LT_OK);
    assert((observed.phases == std::vector<std::string>{"hashing", "extracting", "verifying"}));
    for (const auto &file : entries) {
        auto path = root / "base" / file.first;
        assert(fs::file_size(path) == file.second.size());
        assert(read(path) == file.second);
        assert((fs::status(path).permissions() & fs::perms::owner_write) != fs::perms::none);
    }
    struct stat physical {};
    assert(stat((root / "base/nand/allzero").c_str(), &physical) == 0);
    assert(uint64_t(physical.st_blocks) * 512 < uint64_t(physical.st_size) / 4);
    uint64_t allocated = uint64_t(physical.st_blocks) * 512;
    discard(root / "base");
    auto no_staging = [&] {
        assert(!fs::exists(destination));
        for (const auto &entry : fs::directory_iterator(root))
            assert(entry.path().filename().string().find(".lt-import-") != 0);
    };
    for (const char *phase : {"hashing", "extracting", "verifying"}) {
        Observation cancel;
        cancel.cancel = phase;
        cancel.after_progress = true;
        assert(lt_import_archive_observed(&config, Observation::notify, &cancel, &error) == LT_CANCELLED);
        no_staging();
    }
    // Authentic archive checksum but payload beyond the checked index.
    archive(input, entries, "unexpected");
    expected = sha(read(input));
    config.expected_sha256 = expected.c_str();
    assert(lt_import_archive(&config, &error) == LT_INVALID);
    no_staging();
    // Truncated zlib payload with its new, correct archive checksum.
    std::string truncated = read(input);
    truncated.resize(truncated.size() - 3);
    put(input, truncated);
    expected = sha(truncated);
    config.expected_sha256 = expected.c_str();
    assert(lt_import_archive(&config, &error) != LT_OK);
    no_staging();
    // Extraction bytes are valid but the authoritative NAND listing is wrong.
    auto corrupt = entries;
    corrupt[0].second.replace(corrupt[0].second.find(sha(listing)), 64, std::string(64, '0'));
    archive(input, corrupt);
    expected = sha(read(input));
    config.expected_sha256 = expected.c_str();
    assert(lt_import_archive(&config, &error) == LT_INVALID);
    no_staging();
    // Full 4160-byte n72 pages become the record's NAND container (nand/nand.qcow2), each record stored XOR the n72
    // blank at its place in the padded layout, in any archive order, with the source lock kept beside the rewritten
    // one.
    std::map<std::string, std::string> pages{{"cs0/0.page", std::string(4160, 'a')},
                                             {"cs1/7.page", std::string(4160, '\0')},
                                             {"cs3/524287.page", std::string(4159, '\0') + "z"}};
    std::string page_listing;
    for (const auto &page : pages)
        page_listing += page.first + " " + sha(page.second) + "\n";
    auto packed_lock = [&](const std::string &board, const std::string &listing_sha) {
        return "{\"format\":1,\"board\":\"" + board + "\",\"build\":\"synthetic\",\"outputs\":{"
               "\"nor\":{\"path\":\"nor.bin\",\"sha256\":\"" + sha(nor) +
               "\"},\"iboot\":{\"path\":\"iBoot.bin\",\"sha256\":\"" + sha(boot) +
               "\"},\"kboot\":{\"path\":\"kboot.bin\",\"sha256\":\"" + sha(boot) +
               "\"},\"gid_blobs\":{\"path\":\"gid-blobs.bin\",\"sha256\":\"" + sha(gid) +
               "\"},\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" + listing_sha + "\",\"pages\":3}}}";
    };
    auto packed_entries = [&](const std::string &lock_text) {
        std::vector<std::pair<std::string, std::string>> list{{"nand/cs3/524287.page", pages.at("cs3/524287.page")},
                                                             {"device.lock.json", lock_text}, {"nor.bin", nor},
                                                             {"iBoot.bin", boot}, {"kboot.bin", boot},
                                                             {"gid-blobs.bin", gid},
                                                             {"nand/cs0/0.page", pages.at("cs0/0.page")},
                                                             {"nand/cs1/7.page", pages.at("cs1/7.page")}};
        return list;
    };
    const std::string n72_lock = packed_lock("n72ap", sha(page_listing));
    archive(input, packed_entries(n72_lock));
    expected = sha(read(input));
    config.expected_sha256 = expected.c_str();
    Observation packing;
    assert(lt_import_archive_observed(&config, Observation::notify, &packing, &error) == LT_OK);
    assert((packing.phases == std::vector<std::string>{"hashing", "extracting", "packing", "verifying"}));
    assert(fs::is_regular_file(root / "base/nand/nand.qcow2") && fs::is_regular_file(root / "base/nand/geometry.json") &&
           read(root / "base/source-device.lock.json") == n72_lock);
    const std::string image = read(root / "base/nand/nand.qcow2");
    auto be64 = [&](uint64_t at) {
        uint64_t v = 0;
        for (int i = 0; i < 8; ++i)
            v = v << 8 | uint8_t(image[at + i]);
        return v;
    };
    // A record as the container holds it: chip c's page p at c * chip stride + its block's stride (9 64 KiB
    // clusters) + its place in the block, through the image's own L1 and L2 tables.
    auto record = [&](uint64_t chip, uint64_t page) {
        const uint64_t cluster = 65536, block_stride = 9 * cluster;
        const uint64_t at = chip * 4096 * block_stride + page / 128 * block_stride + page % 128 * 4160;
        std::string out;
        for (uint64_t g = at; g < at + 4160; ++g) {
            const uint64_t l2 = be64(be64(40) + g / cluster / 8192 * 8) & ~(1ull << 63);
            const uint64_t host = l2 ? be64(l2 + g / cluster % 8192 * 8) & ~(1ull << 63) : 0;
            out += host ? image[host + g % cluster] : '\0';
        }
        out[4096 + 8] ^= char(0xff);  // the n72 blank, which every stored record is XOR'd with
        out[4096 + 10] ^= char(0xff);
        return out;
    };
    assert(record(3, 524287) == pages.at("cs3/524287.page") && record(0, 0) == pages.at("cs0/0.page") &&
           record(1, 7) == pages.at("cs1/7.page") && record(0, 1) == std::string(4160, '\0').replace(4104, 1, "\xff")
                                                                       .replace(4106, 1, "\xff"));
    const std::string rewritten = read(root / "base/device.lock.json");
    assert(rewritten.find("\"nand.qcow2\":\"" + sha(image) + "\"") != std::string::npos &&
           rewritten.find("\"source_listing_sha256\":\"" + sha(page_listing) + "\"") != std::string::npos &&
           rewritten.find("listing_sha256\":\"" + sha(page_listing) + "\",\"path\"") == std::string::npos);
    const std::string state = (root / "packed-state").string();
    lt_device_config device{sizeof(device), 1, destination.c_str(), state.c_str(), 0};
    lt_device *opened = nullptr;
    assert(lt_device_open(&device, &opened, &error) == LT_OK);
    lt_device_close(opened);
    discard(root / "base");
    discard(state);
    // The pages must still match the source listing, and only n72 has them.
    for (const auto &bad_lock : {packed_lock("n72ap", std::string(64, '0')), packed_lock("n81ap", sha(page_listing))}) {
        archive(input, packed_entries(bad_lock));
        expected = sha(read(input));
        config.expected_sha256 = expected.c_str();
        assert(lt_import_archive(&config, &error) == LT_INVALID);
        no_staging();
    }
    discard(root);
    std::cout << "production archive importer: sparse/mixed/trailing/empty integrity, three phase cancellations, "
                 "malformed cleanup PASS; allzero allocated=" << allocated << " bytes\n";
}
