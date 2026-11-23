// Device transfer (Transfer.cpp): a record exported and imported again, sparse files included (one over 4 GiB, so
// ZIP64 sizes and offsets are exercised), through files and through a pipe; and the refusals: a damaged, truncated,
// foreign or cancelled transfer leaves nothing behind, and a running device can't be exported.
#include "lighttouch.h"
#include "../src/Sha256.hpp"
#include <cassert>
#include <chrono>
#include <cstring>
#include <fcntl.h>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <map>
#include <set>
#include <string>
#include <sys/stat.h>
#include <thread>
#include <unistd.h>
#include <vector>
namespace fs = std::filesystem;

static void put(const fs::path &path, const std::string &bytes) {
    fs::create_directories(path.parent_path());
    std::ofstream out(path, std::ios::binary);
    out.write(bytes.data(), std::streamsize(bytes.size()));
    assert(out.good());
}
static std::string read(const fs::path &path) {
    std::ifstream in(path, std::ios::binary);
    assert(in.good());
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}
static std::string digest(const std::string &bytes) {
    lt::Sha256 h;
    h.update(bytes.data(), bytes.size());
    return h.finish();
}
static std::string file_digest(const fs::path &path) {
    int fd = open(path.c_str(), O_RDONLY);
    assert(fd >= 0);
    lt::Sha256 h;
    std::vector<char> buffer(1 << 20);
    for (ssize_t n; (n = read(fd, buffer.data(), buffer.size())) > 0;)
        h.update(buffer.data(), size_t(n));
    close(fd);
    return h.finish();
}
// A sparse file: [size] bytes, data only at [spots].
static void sparse(const fs::path &path, uint64_t size, const std::vector<std::pair<uint64_t, std::string>> &spots) {
    fs::create_directories(path.parent_path());
    int fd = open(path.c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0600);
    assert(fd >= 0 && ftruncate(fd, off_t(size)) == 0);
    for (const auto &s : spots)
        assert(pwrite(fd, s.second.data(), s.second.size(), off_t(s.first)) == ssize_t(s.second.size()));
    close(fd);
}
static uint64_t allocated(const fs::path &path) {
    struct stat st {};
    assert(stat(path.c_str(), &st) == 0);
    return uint64_t(st.st_blocks) * 512;
}
static std::map<std::string, std::string> tree(const fs::path &root) {
    std::map<std::string, std::string> files;
    for (const auto &e : fs::recursive_directory_iterator(root))
        if (e.is_regular_file())
            files[e.path().lexically_relative(root).generic_string()] = file_digest(e.path());
    return files;
}
static std::vector<std::string> stages(const fs::path &library) {
    std::vector<std::string> found;
    for (const auto &e : fs::directory_iterator(library / "records"))
        if (e.path().filename().string()[0] == '.')
            found.push_back(e.path().filename().string());
    return found;
}
struct Progress {
    std::string phases;
    uint64_t last_done = 0, total = 0, cancel_after = UINT64_MAX;
    static int notify(void *opaque, const char *phase, uint64_t done, uint64_t total) {
        auto *p = static_cast<Progress *>(opaque);
        if (p->phases.empty() || p->phases.rfind(phase) != p->phases.size() - strlen(phase))
            p->phases += std::string(p->phases.empty() ? "" : ",") + phase;
        p->last_done = done;
        p->total = total;
        return done >= p->cancel_after ? 1 : 0;
    }
};
static lt_library *open_library(const fs::path &root) {
    const std::string path = root.string();
    lt_library_config config{sizeof(config), 1, path.c_str()};
    lt_library *library = nullptr;
    lt_error error{sizeof(error), 1, 0, {}};
    assert(lt_library_open(&config, &library, &error) == LT_OK);
    return library;
}
static lt_status export_to(lt_library *l, const std::string &id, const fs::path &path, Progress *progress,
                           lt_error &error) {
    const std::string out = path.string();
    lt_export_config config{sizeof(config), 1, id.c_str(), out.c_str(), -1, "LightTouch test"};
    return lt_library_export(l, &config, progress ? Progress::notify : nullptr, progress, &error);
}
static lt_status import_from(lt_library *l, const fs::path &path, Progress *progress, lt_record_info &info,
                             lt_error &error) {
    const std::string in = path.string();
    lt_transfer_import_config config{sizeof(config), 1, in.c_str(), -1};
    return lt_library_import_device(l, &config, progress ? Progress::notify : nullptr, progress, &info, &error);
}
static uint32_t count(lt_library *l) {
    uint32_t n = 0;
    lt_error error{sizeof(error), 1, 0, {}};
    assert(lt_library_count(l, &n, &error) == LT_OK);
    return n;
}

int main() {
    const fs::path root = fs::absolute(fs::temp_directory_path() / ("lt-transfer-test-" + std::to_string(getpid())));
    struct Cleanup {
        fs::path root;
        ~Cleanup() {
            std::error_code ec;
            fs::remove_all(root, ec);
        }
    } cleanup{root};
    fs::create_directories(root);

    // A prepared base (the 2G's iboot route), imported into library A as a record.
    const fs::path base = root / "prepared";
    const std::string page_a(131089, 'a'), nor(65536, 'n'), iboot = "boot", gid = "gid", identity = "{\"seed\":1}";
    put(base / "nand/a.pages", page_a);
    put(base / "nor.bin", nor);
    put(base / "iBoot.bin", iboot);
    put(base / "gid-blobs.bin", gid);
    put(base / "identity.json", identity);
    const std::string listing = "a.pages " + digest(page_a) + "\n";
    put(base / "device.lock.json",
        "{\"format\":1,\"board\":\"n72ap\",\"build\":\"7E18\",\"boot_strategy\":\"iboot\",\"outputs\":{"
        "\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" + digest(listing) + "\",\"pages\":1},"
        "\"nor\":{\"path\":\"nor.bin\",\"sha256\":\"" + digest(nor) + "\"},"
        "\"iboot\":{\"path\":\"iBoot.bin\",\"sha256\":\"" + digest(iboot) + "\"},"
        "\"gid_blobs\":{\"path\":\"gid-blobs.bin\",\"sha256\":\"" + digest(gid) + "\"}},"
        "\"identity\":{\"sha256\":\"" + digest(identity) + "\"}}");
    put(root / "rom.bin", std::string(65536, 'r'));
    lt_library *a = open_library(root / "a");
    lt_error error{sizeof(error), 1, 0, {}};
    lt_record_info info{sizeof(info), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
    const std::string base_path = base.string(), rom = (root / "rom.bin").string();
    lt_library_import_config import{sizeof(import), 1, base_path.c_str(), nullptr, "Transfer test", rom.c_str(),
                                    nullptr, 0, nullptr, nullptr};
    assert(lt_library_import(a, &import, &info, &error) == LT_OK);
    const std::string id = info.id;
    const fs::path record = root / "a/records" / id;

    // A running device can't be exported.
    {
        lt_device *device = nullptr;
        assert(lt_library_device_open(a, id.c_str(), 1, &device, &error) == LT_OK);
        assert(export_to(a, id, root / "busy.zip", nullptr, error) == LT_BUSY);
        assert(!fs::exists(root / "busy.zip") && !fs::exists(root / "busy.zip.partial"));
        lt_device_close(device);
    }
    // Its state as a used device leaves it: a 5 GiB sparse NAND overlay with data past 4 GiB, a NOR, a SIM; and what
    // an export leaves out (this host's admission cache, logs, the guest-offer staging, temporaries).
    const uint64_t big = 5ull << 30;
    sparse(record / "state/nand/nand.qcow2", big,
           {{0, std::string(4096, 'q')}, {3ull << 30, std::string(70000, 'w')}, {(4ull << 30) + 12345, "past 4 GiB"},
            {big - 3, "end"}});
    put(record / "state/nor.bin", std::string(65536 - 8, '\0') + "nor tail");
    put(record / "state/sim", "pin=1234");
    put(record / "state/guest-verdicts", "good 47\n");
    put(record / "state/.verified-digests", "/elsewhere|1|2 deadbeef\n");
    put(record / "state/.base-identity", "x");
    put(record / "state/serial.log", "console");
    put(record / "state/work/guest-offer/offer", "offer");
    put(record / "state/.nand-clone.tmp/x", "tmp");
    put(record / "carrier.json", "{\"carrier\":\"Test\"}\n");

    put(record / "state/.base-identity", "x");  // the open pinned it; an export must not carry it either way
    put(record / "state/.dirty", "schema=1\n");

    // Export, with progress.
    Progress exported;
    const auto started = std::chrono::steady_clock::now();
    assert(export_to(a, id, root / "device.zip", &exported, error) == LT_OK);
    std::cout << "export " << std::chrono::duration<double>(std::chrono::steady_clock::now() - started).count()
              << " s, " << fs::file_size(root / "device.zip") << " bytes\n";
    assert(exported.phases == "exporting" && exported.last_done == exported.total && exported.total > big);
    assert(!fs::exists(root / "device.zip.partial"));
    {
        const std::string bytes = read(root / "device.zip");
        assert(bytes.compare(0, 4, "PK\x03\x04") == 0 && bytes.find("lighttouch-device.json") == 30);
    }

    // Import into another library, twice: a fresh id each time, the second named " (2)".
    lt_library *b = open_library(root / "b");
    Progress imported;
    lt_record_info first{sizeof(first), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
    const auto import_started = std::chrono::steady_clock::now();
    assert(import_from(b, root / "device.zip", &imported, first, error) == LT_OK);
    std::cout << "import " << std::chrono::duration<double>(std::chrono::steady_clock::now() - import_started).count()
              << " s\n";
    assert(imported.phases == "importing,finishing");
    assert(std::string(first.id) != id && std::string(first.display_name) == "Transfer test" && first.selected &&
           first.dirty);
    lt_record_info second{sizeof(second), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
    assert(import_from(b, root / "device.zip", nullptr, second, error) == LT_OK);
    assert(std::string(second.display_name) == "Transfer test (2)" && !second.selected);
    lt_record_info third{sizeof(third), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
    assert(import_from(b, root / "device.zip", nullptr, third, error) == LT_OK);
    assert(std::string(third.display_name) == "Transfer test (3)");

    // The same files, byte for byte, but for what stays behind; sparse files stay sparse.
    const fs::path copy = root / "b/records" / first.id;
    auto want = tree(record), got = tree(copy);
    for (const char *left : {"state/.verified-digests", "state/.base-identity", "state/.lease", "state/serial.log",
                             "state/work/guest-offer/offer", "state/.nand-clone.tmp/x", "record.json"})
        want.erase(left);
    for (const char *made : {"state/.verified-digests", "state/.base-identity", "record.json"}) {
        assert(got.count(made));
        got.erase(made);
    }
    assert(want == got);
    assert(fs::file_size(copy / "state/nand/nand.qcow2") == big && allocated(copy / "state/nand/nand.qcow2") < (16u << 20));
    assert(read(copy / "state/.base-identity") == info.base_sha256);
    assert(read(copy / "state/.verified-digests").find((root / "b/records").string()) != std::string::npos);
    assert(stages(root / "b").empty());

    // It opens (dirty, so with recovery), its base admitted from the digests taken on the way in.
    {
        lt_device *device = nullptr;
        assert(lt_library_device_open(b, first.id, 0, &device, &error) == LT_DIRTY);
        if (lt_library_device_open(b, first.id, 1, &device, &error) != LT_OK)
            std::cerr << "open: " << error.message << "\n";
        assert(error.code == LT_OK);
        lt_device_close(device);
    }

    // Through pipes: the exporter writes one, the importer reads another.
    {
        int out[2], in[2];
        assert(pipe(out) == 0 && pipe(in) == 0);
        std::string streamed;
        std::thread drain([&] {
            char buffer[65536];
            for (ssize_t n; (n = read(out[0], buffer, sizeof buffer)) > 0;)
                streamed.append(buffer, size_t(n));
        });
        lt_export_config config{sizeof(config), 1, id.c_str(), nullptr, out[1], "LightTouch test"};
        assert(lt_library_export(a, &config, nullptr, nullptr, &error) == LT_OK);
        close(out[1]);
        drain.join();
        close(out[0]);
        assert(streamed.size() > 1000);
        std::thread feed([&] {
            for (size_t at = 0; at < streamed.size();) {
                ssize_t n = write(in[1], streamed.data() + at, std::min<size_t>(65536, streamed.size() - at));
                if (n <= 0)
                    break;
                at += size_t(n);
            }
            close(in[1]);
        });
        lt_transfer_import_config import_config{sizeof(import_config), 1, nullptr, in[0]};
        lt_record_info piped{sizeof(piped), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
        assert(lt_library_import_device(b, &import_config, nullptr, nullptr, &piped, &error) == LT_OK);
        feed.join();
        close(in[0]);
        assert(tree(root / "b/records" / piped.id).at("state/nand/nand.qcow2") == got.at("state/nand/nand.qcow2"));
    }

    // Refusals leave no stage and no record.
    const uint32_t records = count(b);
    const std::string bytes = read(root / "device.zip");
    auto refused = [&](const std::string &name, const std::string &content, lt_status status, const char *message) {
        put(root / name, content);
        lt_record_info ignored{sizeof(ignored), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
        const lt_status got_status = import_from(b, root / name, nullptr, ignored, error);
        if (got_status != status || std::string(error.message).find(message) == std::string::npos)
            std::cerr << name << ": " << got_status << " " << error.message << "\n";
        assert(got_status == status && std::string(error.message).find(message) != std::string::npos);
        assert(count(b) == records && stages(root / "b").empty());
    };
    std::string damaged = bytes;
    // Several bytes: a lone flipped bit can land in a deflate block's padding, which no reader looks at.
    for (size_t i = 0; i < 64; ++i)
        damaged[damaged.size() / 2 + i] ^= 0x55;
    refused("damaged.zip", damaged, LT_INVALID, "damaged");
    refused("truncated.zip", bytes.substr(0, bytes.size() / 2), LT_INVALID, "ends early");
    refused("foreign.zip", std::string("PK\x03\x04", 4) + std::string(22, '\0') + std::string("\x05\0\0\0a.txt", 9),
            LT_UNSUPPORTED, "isn't a device exported");
    refused("empty.zip", "", LT_INVALID, "ends early");

    // Cancelled halfway: no record, no stage; a cancelled export leaves no file.
    {
        Progress cancel;
        cancel.cancel_after = 1u << 30;
        lt_record_info ignored{sizeof(ignored), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
        assert(import_from(b, root / "device.zip", &cancel, ignored, error) == LT_CANCELLED);
        assert(count(b) == records && stages(root / "b").empty());
        Progress stop;
        stop.cancel_after = 1u << 30;
        assert(export_to(a, id, root / "cancelled.zip", &stop, error) == LT_CANCELLED);
        assert(!fs::exists(root / "cancelled.zip") && !fs::exists(root / "cancelled.zip.partial"));
    }

    // A stage an import left when its process died is swept by the next import.
    fs::create_directories(root / "b/records/.record-import-dead00");
    put(root / "b/records/.record-import-dead00/.owner", "");
    put(root / "b/records/.record-import-dead00/base/x", "partial");
    {
        lt_record_info again{sizeof(again), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
        assert(import_from(b, root / "device.zip", nullptr, again, error) == LT_OK);
        assert(stages(root / "b").empty());
    }
    lt_library_close(a);
    lt_library_close(b);
    std::cout << "transfer ok\n";
    return 0;
}
