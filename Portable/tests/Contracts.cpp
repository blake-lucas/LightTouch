#include "lighttouch.h"
#include "../src/Sha256.hpp"
#include "../src/Json.hpp"
#include "../src/MotionClock.hpp"
#include "../src/Base64.hpp"
#include "../src/GuestNetwork.hpp"
#include "../src/Elapsed.hpp"
#include "../src/Lifecycle.hpp"
#include <zlib.h>
#include <cassert>
#include <cmath>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <unistd.h>
#include <sys/stat.h>
namespace fs = std::filesystem;
static void put(const fs::path &p, const std::string &s) {
    fs::create_directories(p.parent_path());
    std::ofstream f(p, std::ios::binary);
    f.write(s.data(), s.size());
    assert(f.good());
}
static std::string hash(const std::string &s) {
    lt::Sha256 h;
    h.update(s.data(), s.size());
    return h.finish();
}
static void archive(const fs::path &path, const std::string &index, const std::string &data,
                    const std::string &tail = "") {
    std::string blob = "ITPACK01";
    uint32_t n = uint32_t(index.size());
    for (int i = 0; i < 4; i++)
        blob += char(n >> (i * 8));
    blob += index;
    uLongf cap = compressBound(data.size());
    std::string stream(cap, '\0');
    assert(compress2(reinterpret_cast<Bytef *>(stream.data()), &cap, reinterpret_cast<const Bytef *>(data.data()),
                     data.size(), 6) == Z_OK);
    stream.resize(cap);
    put(path, blob + stream + tail);
}
static std::string sha_file(const fs::path &p) {
    char h[65];
    lt_error e{sizeof(e), 1, 0, {}};
    assert(lt_sha256_file(p.c_str(), h, &e) == LT_OK);
    return h;
}
int main() {
    {
        std::array<int16_t, 8192> output{};
        output[0] = 123;
        lt_pcm_info info{sizeof(info), 1, 9, 8, 7, 0, 6, 5};
        const auto before = info;
        lt_error error{sizeof(error), 1, 0, {}};
        assert(lt_runtime_pcm(nullptr, output.data(), 4096, &info, &error) == LT_INVALID);
        assert(error.code == LT_INVALID && std::string(error.message) == "invalid PCM output");
        assert(output[0] == 123 && memcmp(&info, &before, sizeof(info)) == 0);
        // The invalid runtime/output check keeps precedence over invalid info/capacity.
        assert(lt_runtime_pcm(nullptr, nullptr, 0, nullptr, &error) == LT_INVALID);
        assert(std::string(error.message) == "invalid PCM output");
        assert(lt_runtime_pcm(nullptr, output.data(), 4096, &info, nullptr) == LT_INVALID);
    }
    // A zero engine return is not a guest shutdown receipt. Finalization may
    // briefly retain dirty while the confirmed publisher is still cleaning up.
    assert(lt::terminal_issue(true, true, false, true, 0) == lt::TerminalIssue::unconfirmed_shutdown);
    assert(lt::terminal_issue(true, false, false, true, 0) == lt::TerminalIssue::unconfirmed_shutdown);
    assert(lt::terminal_issue(true, false, true, true, 0) == lt::TerminalIssue::dirty_state);
    assert(lt::terminal_issue(true, true, true, true, 0) == lt::TerminalIssue::none);
    assert(lt::terminal_issue(true, false, true, false, 0) == lt::TerminalIssue::none);
    assert(lt::terminal_issue(true, false, false, true, 1) == lt::TerminalIssue::engine_exit);
    // Stop is admitted after the loop timestamp is sampled. Unsigned subtraction
    // with that stale snapshot used to kill a real worker in the same iteration.
    const uint64_t loop_snapshot = 90000000000ull;
    const uint64_t stop_admitted = loop_snapshot + 1000000;
    assert(loop_snapshot - stop_admitted > lt::parent_shutdown_timeout_ns);
    assert(!lt::elapsed_exceeds(loop_snapshot, stop_admitted, lt::parent_shutdown_timeout_ns));
    assert(!lt::elapsed_exceeds(stop_admitted + 1, stop_admitted, lt::parent_shutdown_timeout_ns));
    const uint64_t worker_deadline = stop_admitted + lt::worker_shutdown_timeout_ns;
    assert(!lt::elapsed_exceeds(worker_deadline, stop_admitted, lt::worker_shutdown_timeout_ns));
    assert(lt::elapsed_exceeds(worker_deadline + 1, stop_admitted, lt::worker_shutdown_timeout_ns));
    assert(!lt::elapsed_exceeds(worker_deadline + 1, stop_admitted, lt::parent_shutdown_timeout_ns));
    const uint64_t parent_deadline = stop_admitted + lt::parent_shutdown_timeout_ns;
    assert(!lt::elapsed_exceeds(parent_deadline, stop_admitted, lt::parent_shutdown_timeout_ns));
    assert(lt::elapsed_exceeds(parent_deadline + 1, stop_admitted, lt::parent_shutdown_timeout_ns));
    char temp[] = "/tmp/lighttouch-contract-XXXXXX";
    assert(mkdtemp(temp));
    fs::path root = temp;
    lt_error e{sizeof(e), 1, 0, {}};
    assert(hash("") == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert(hash("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert(hash(std::string(1000000, 'a')) == "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    bool bad = false;
    try {
        lt::parse("{\"x\":1,\"x\":2}");
    } catch (...) {
        bad = true;
    }
    assert(bad);
    std::string tool_body;
    assert(lt::network_tool_chunk("0 0 SGk=", true, tool_body) == "0 0 755 /tmp/lighttouch-ittrust" && tool_body == "Hi");
    assert(lt::network_tool_chunk("2048 1 SGk=", false, tool_body) == "2048 1 755 /tmp/lighttouch-itproxy");
    for (const auto &invalid : {"", "0 2 SGk=", "-1 1 SGk=", "0 1 ", "0 1 SGk= /tmp/other", "1048576 1 SGk=", "9999999999999999999 1 SGk="}) {
        bool refused = false;
        try { lt::network_tool_chunk(invalid, true, tool_body); } catch (...) { refused = true; }
        assert(refused);
    }
    bad = false;
    try {
        lt::parse("{\"x\":18446744073709551616}").at("x").integer();
    } catch (...) {
        bad = true;
    }
    assert(bad);
    double x, y;
    assert(lt_map_contact(20, 30, 10, 10, 20, 40, 90, &x, &y) == LT_OK && x == .5 && y == .5);
    assert(lt_map_contact(0, 0, 10, 10, 20, 40, 0, &x, &y) == LT_AGAIN);
    assert(lt_map_contact(NAN, 0, 0, 0, 20, 40, 0, &x, &y) == LT_INVALID);
    lt_motion raw{sizeof(raw), 1, 0, 3, 0, {1, 2, 3}, {4, 5, 6}}, mapped{sizeof(mapped), 1, 0, 0, 0, {}, {}};
    assert(lt_map_motion(90, 900000000, 1000000000, &raw, &mapped, &e) == LT_OK);
    assert(mapped.acceleration[0] == -2 && mapped.acceleration[1] == 1 && mapped.angular_velocity[0] == -5);
    assert(lt_map_motion(0, 1, 1000000000, &raw, &mapped, &e) == LT_AGAIN);
    assert(lt::base64_encode("Hi") == "SGk=" && lt::base64_decode("SGk=") == "Hi");
    assert(lt::base64_decode(lt::base64_encode("multiline\ntext")) == "multiline\ntext");
    bad = false;
    try {
        lt::base64_decode("YR==");
    } catch (...) {
        bad = true;
    }
    assert(bad);
    lt::MotionClock clocks;
    clocks.validate(1, 1000000000, 1000000000);
    clocks.commit(1, 1000000000);
    clocks.validate(2, 1000000000, 1000000000);
    clocks.commit(2, 1000000000);
    for (uint64_t t = 1005000000; t < 1500000000; t += 5000000) {
        clocks.validate(1, t, t);
        clocks.commit(1, t);
        clocks.validate(2, t, t);
        clocks.commit(2, t);
    }
    bad = false;
    try {
        clocks.validate(3, 1495000001, 1495000001);
    } catch (...) {
        bad = true;
    }
    assert(bad);
    clocks.reset();
    clocks.validate(3, 1000000000, 1000000000);
    const std::string nor = "nor", page = "page", iboot = "boot", gid = "gid";
    std::string listing = "cs0/0.page " + hash(page) + "\n";
    std::string lock = "{\"format\":1,\"board\":\"n72ap\",\"build\":\"7E18\",\"boot_strategy\":\"iboot\",\"outputs\":{"
                       "\"nor\":{\"path\":\"nor.bin\",\"sha256\":\"" +
                       hash(nor) + "\"},\"iboot\":{\"path\":\"iBoot.bin\",\"sha256\":\"" + hash(iboot) +
                       "\"},\"gid_blobs\":{\"path\":\"gid-blobs.bin\",\"sha256\":\"" + hash(gid) +
                       "\"},\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" + hash(listing) +
                       "\",\"pages\":1}},\"machine\":{\"ecid\":\"0x123\"}}";
    std::vector<std::pair<std::string, std::string>> entries = {{"device.lock.json", lock},
                                                                {"nor.bin", nor},
                                                                {"nand/cs0/0.page", page},
                                                                {"iBoot.bin", iboot},
                                                                {"gid-blobs.bin", gid}};
    std::string index = "{\"entries\":[", data;
    for (size_t i = 0; i < entries.size(); i++) {
        if (i)
            index += ",";
        index += "{\"name\":\"" + entries[i].first + "\",\"size\":" + std::to_string(entries[i].second.size()) +
                 ",\"mode\":420}";
        data += entries[i].second;
    }
    index += "]}";
    archive(root / "base.itbase", index, data);
    std::string sha = sha_file(root / "base.itbase"), dest = (root / "base").string();
    lt_import_config config{sizeof(config), 1,      (root / "base.itbase").c_str(), dest.c_str(), sha.c_str(), 0,
                            nullptr,        nullptr};
    std::string source = (root / "base.itbase").string();
    config.source = source.c_str();
    assert(lt_import_archive(&config, &e) == LT_OK);
    // Imported files keep ordinary modes: the lock's hashes vouch for a base, not its permissions.
    assert((fs::status(root / "base/nor.bin").permissions() & fs::perms::owner_write) != fs::perms::none);
    std::string state = (root / "state").string();
    lt_device_config dc{sizeof(dc), 1, dest.c_str(), state.c_str(), 0};
    lt_device *d = nullptr, *second = nullptr;
    assert(lt_device_open(&dc, &d, &e) == LT_OK);
    assert(lt_device_open(&dc, &second, &e) == LT_BUSY);
    put(root / "rom", "rom");
    std::string rom = (root / "rom").string();
    lt_boot_config boot{sizeof(boot), 1, "iPod-Touch-2G", rom.c_str(), nullptr, nullptr, 0};
    int argc;
    const char *const *argv;
    assert(lt_device_boot_argv(d, &boot, &argc, &argv, &e) == LT_OK);
    std::string args = argv[2];
    assert(args.find("nandrw=" + state + "/nand") != std::string::npos && args.find("ecid=0x123") != std::string::npos);
    lt_device_close(d);
    put(root / "state/.dirty", "crash");
    assert(lt_device_open(&dc, &d, &e) == LT_DIRTY);
    dc.allow_dirty = 1;
    assert(lt_device_open(&dc, &d, &e) == LT_OK);
    assert(fs::exists(root / "state/.dirty"));
    lt_device_close(d);

    // Prepared n81 uses kboot, full NAND/NOR and the guest package family. No
    // independent ROM or n72 memory setting may leak into its recipe.
    std::string n81lock = "{\"format\":1,\"board\":\"n81ap\",\"build\":\"8C148\",\"boot_strategy\":\"kboot\","
                          "\"outputs\":{\"nor\":{\"path\":\"nor.bin\",\"sha256\":\"" +
                          hash(nor) + "\"},\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" + hash(listing) +
                          "\",\"pages\":1},\"kboot\":{\"path\":\"kboot.bin\",\"sha256\":\"" + hash(iboot) + "\"}}}";
    put(root / "n81source/device.lock.json", n81lock);
    put(root / "n81source/nand/cs0/0.page", page);
    put(root / "n81source/nor.bin", nor);
    put(root / "n81source/kboot.bin", iboot);
    std::string n81source = (root / "n81source").string(), n81base = (root / "n81base").string(),
                n81state = (root / "n81state").string();
    lt_import_config n81import{sizeof(n81import), 1, n81source.c_str(), n81base.c_str(), nullptr, 0, nullptr, nullptr};
    assert(lt_import_directory(&n81import, &e) == LT_OK);
    lt_device_config n81dc{sizeof(n81dc), 1, n81base.c_str(), n81state.c_str(), 0};
    assert(lt_device_open(&n81dc, &d, &e) == LT_OK);
    std::string manifest = "{\"format\":1,\"serial\":26,\"version\":\"1.1\",\"requires\":{\"boards\":[\"n81ap\"],"
                           "\"builds\":[\"8*\"],\"host\":{\"guest-package\":[1,1]}},\"hooks\":[],\"jobs\":[],\"files\":"
                           "[{\"name\":\"bin/tool\",\"mode\":\"755\",\"size\":4,\"sha256\":\"" +
                           hash("tool") + "\"}]}";
    std::string packindex =
        "{\"entries\":[{\"name\":\"k48-ios4/manifest.json\",\"size\":" + std::to_string(manifest.size()) +
        "},{\"name\":\"k48-ios4/bin/tool\",\"size\":4}]}";
    archive(root / "synthetic.itpack", packindex, manifest + "tool");
    std::string packpath = (root / "synthetic.itpack").string();
    lt_boot_config n81boot{sizeof(n81boot), 1, "iPod-Touch-4G", nullptr, packpath.c_str(), nullptr, 0};
    assert(lt_device_boot_argv(d, &n81boot, &argc, &argv, &e) == LT_OK);
    args = argv[2];
    assert(args.find("kboot=" + n81base + "/kboot.bin") != std::string::npos);
    assert(args.find("nand-overlay=" + n81state + "/nand") != std::string::npos);
    assert(args.find("nor-rw=" + n81state + "/nor.bin") != std::string::npos);
    assert(args.find("bootrom=") == std::string::npos);
    for (int i = 0; i < argc; i++)
        assert(std::string(argv[i]) != "-m");
    assert(fs::exists(root / "n81state/work/guest-offer/bin/tool"));
    assert(args.find("headphones=") == std::string::npos);
    n81boot.headphones = 1;
    assert(lt_device_boot_argv(d, &n81boot, &argc, &argv, &e) == LT_OK);
    assert(std::string(argv[2]).find(",headphones=on") != std::string::npos);
    lt_device_close(d);

    // The other boards boot too (Light Touch for Mac's BootRecipe): an iPhone 4 from kboot with its modem and the
    // IMEI the lock records, and no NOR (null: a 3.x kboot device has none); an iPod touch 1G from its SecureROM,
    // the NOR a pflash drive.
    auto prepared = [&](const std::string &name, const std::string &lock,
                        const std::vector<std::pair<std::string, std::string>> &files) {
        put(root / (name + "source/device.lock.json"), lock);
        put(root / (name + "source/nand/cs0/0.page"), page);
        for (const auto &f : files)
            put(root / (name + "source") / f.first, f.second);
        std::string from = (root / (name + "source")).string(), to = (root / (name + "base")).string();
        lt_import_config c{sizeof(c), 1, from.c_str(), to.c_str(), nullptr, 0, nullptr, nullptr};
        assert(lt_import_directory(&c, &e) == LT_OK);
        return to;
    };
    std::string n90base = prepared("n90",
        "{\"format\":1,\"board\":\"n90ap\",\"build\":\"8C148\",\"boot_strategy\":\"kboot\",\"machine\":"
        "{\"imei\":\"012345678901234\"},\"outputs\":{\"nor\":null,\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" +
            hash(listing) + "\",\"pages\":1},\"kboot\":{\"path\":\"kboot.bin\",\"sha256\":\"" + hash(iboot) + "\"}}}",
        {{"kboot.bin", iboot}});
    std::string n90state = (root / "n90state").string();
    lt_device_config n90dc{sizeof(n90dc), 1, n90base.c_str(), n90state.c_str(), 0};
    assert(lt_device_open(&n90dc, &d, &e) == LT_OK);
    lt_boot_config n90boot{sizeof(n90boot), 1, "iPhone-4", nullptr, nullptr, nullptr, 0};
    assert(lt_device_boot_argv(d, &n90boot, &argc, &argv, &e) == LT_OK);
    args = argv[2];
    assert(args.find("kboot=" + n90base + "/kboot.bin") != std::string::npos);
    assert(args.find("baseband=on") != std::string::npos && args.find("imei=012345678901234") != std::string::npos);
    assert(args.find("nor-rw=") == std::string::npos && args.find("bootrom=") == std::string::npos);
    lt_device_close(d);
    std::string n45base = prepared("n45",
        "{\"format\":1,\"board\":\"n45ap\",\"build\":\"3A101a\",\"machine\":{\"wifi-mac\":\"\"},\"outputs\":{"
        "\"nor\":{\"path\":\"nor.bin\",\"sha256\":\"" + hash(nor) + "\"},\"iboot\":{\"path\":\"iBoot.bin\",\"sha256\":\"" +
            hash(iboot) + "\"},\"nand\":{\"path\":\"nand\",\"listing_sha256\":\"" + hash(listing) + "\",\"pages\":1}}}",
        {{"nor.bin", nor}, {"iBoot.bin", iboot}});
    std::string n45state = (root / "n45state").string();
    lt_device_config n45dc{sizeof(n45dc), 1, n45base.c_str(), n45state.c_str(), 0};
    assert(lt_device_open(&n45dc, &d, &e) == LT_OK);
    lt_boot_config n45boot{sizeof(n45boot), 1, "iPod-Touch-1G", nullptr, nullptr, nullptr, 0};
    assert(lt_device_boot_argv(d, &n45boot, &argc, &argv, &e) != LT_OK);  // no boot ROM
    n45boot.bootrom_path = rom.c_str();
    assert(lt_device_boot_argv(d, &n45boot, &argc, &argv, &e) == LT_OK);
    args = argv[2];
    assert(args.find("bootrom=" + rom) != std::string::npos && args.find("iboot=" + n45base + "/iBoot.bin") != std::string::npos);
    assert(args.find("nand-overlay=" + n45state + "/nand") != std::string::npos && args.find("wifi-mac") == std::string::npos);
    bool drive = false;
    for (int i = 0; i + 1 < argc; i++)
        drive |= std::string(argv[i]) == "-drive" && std::string(argv[i + 1]) == "if=pflash,format=raw,file=" + n45state + "/nor.bin";
    assert(drive);
    lt_device_close(d);
    archive(root / "bad.itbase", "{\"entries\":[{\"name\":\"../escape\",\"size\":1,\"mode\":420}]}", "a");
    source = (root / "bad.itbase").string();
    sha = sha_file(source);
    dest = (root / "bad").string();
    config.source = source.c_str();
    config.destination = dest.c_str();
    config.expected_sha256 = sha.c_str();
    assert(lt_import_archive(&config, &e) == LT_INVALID && !fs::exists(dest) && !fs::exists(root / "escape"));
    archive(root / "tail.itbase", index, data, "bad");
    source = (root / "tail.itbase").string();
    sha = sha_file(source);
    dest = (root / "tail").string();
    config.source = source.c_str();
    config.destination = dest.c_str();
    config.expected_sha256 = sha.c_str();
    assert(lt_import_archive(&config, &e) == LT_INVALID && !fs::exists(dest));
    archive(root / "cancel.itbase", index, data);
    source = (root / "cancel.itbase").string();
    sha = sha_file(source);
    dest = (root / "cancel").string();
    config.source = source.c_str();
    config.destination = dest.c_str();
    config.expected_sha256 = sha.c_str();
    config.progress = [](void *, uint64_t, uint64_t) { return 1; };
    assert(lt_import_archive(&config, &e) == LT_CANCELLED && !fs::exists(dest));

    // Firmware output declares its matching package; native code chooses it
    // without requiring UI architecture/recipe knowledge.
    fs::copy_file(root / "synthetic.itpack", root / "n81source/guest.itpack");
    std::string withpack = n81lock;
    size_t outputs = withpack.find("\"outputs\":{");
    assert(outputs != std::string::npos);
    withpack.insert(outputs + 11, "\"guest_package\":{\"path\":\"guest.itpack\",\"sha256\":\"" +
                                      sha_file(root / "n81source/guest.itpack") + "\"},");
    put(root / "n81source/device.lock.json", withpack);
    std::string packedbase = (root / "n81packbase").string();
    n81import.destination = packedbase.c_str();
    assert(lt_import_directory(&n81import, &e) == LT_OK);
    const uint8_t colors[8] = {0, 0, 255, 255, 255, 0, 0, 255};
    lt_frame_info picture{sizeof(picture), 1, 2, 1, 8, 1, 1, 100, 8};
    std::string shot = (root / "contract.bmp").string();
    assert(lt_frame_write_bmp(&picture, colors, sizeof(colors), shot.c_str(), &e) == LT_OK);
    std::ifstream bitmap(shot, std::ios::binary);
    std::string image((std::istreambuf_iterator<char>(bitmap)), {});
    assert(image.size() == 62 && image.substr(0, 2) == "BM" && uint8_t(image[56]) == 255 && uint8_t(image[58]) == 255);
    assert(lt_frame_write_bmp(&picture, colors, 7, shot.c_str(), &e) == LT_INVALID);
    // Native durable record selection keeps both boards and distinct state.
    std::string libroot = (root / "library").string();
    lt_library_config lc{sizeof(lc), 1, libroot.c_str()};
    lt_library *l = nullptr;
    assert(lt_library_open(&lc, &l, &e) == LT_OK);
    lt_record_info first{}, second_record{}, selected_record{};
    first.size = second_record.size = selected_record.size = sizeof(lt_record_info);
    first.version = second_record.version = selected_record.version = 1;
    lt_library_import_config li{
        sizeof(li), 1, (root / "base").c_str(), nullptr, "Baseline", rom.c_str(), nullptr, 0, nullptr, nullptr};
    dest = (root / "base").string();
    li.source_path = dest.c_str();
    assert(lt_library_import(l, &li, &first, &e) == LT_OK);
    li.source_path = packedbase.c_str();
    li.display_name = "Gyro board";
    li.bootrom_path = nullptr;
    li.guest_package_path = nullptr;
    assert(lt_library_import(l, &li, &second_record, &e) == LT_OK);
    assert(*second_record.guest_package_path && fs::exists(second_record.guest_package_path));
    assert(sha_file(second_record.guest_package_path) == sha_file(root / "synthetic.itpack"));
    assert(std::string(first.id) != second_record.id && std::string(first.state_path) != second_record.state_path);
    uint32_t count = 0;
    assert(lt_library_count(l, &count, &e) == LT_OK && count == 2);
    assert(lt_library_select(l, second_record.id, &e) == LT_OK);
    lt_library_close(l);
    assert(lt_library_open(&lc, &l, &e) == LT_OK);
    assert(lt_library_selected(l, &selected_record, &e) == LT_OK &&
           std::string(selected_record.id) == second_record.id);
    assert(lt_library_device_open(l, first.id, 0, &d, &e) == LT_OK);
    lt_device_close(d);
    assert(lt_library_device_open(l, second_record.id, 0, &d, &e) == LT_OK);
    lt_device_close(d);
    assert(lt_library_adopt_legacy(l, &li, state.c_str(), &selected_record, &e) == LT_BUSY);
    assert(lt_library_select(l, "../escape", &e) == LT_INVALID);
    assert(lt_library_count(l, &count, &e) == LT_OK && count == 2);
    // Rename keeps every other field; remove refuses an open device and moves the selection.
    assert(lt_library_rename(l, first.id, "iPod touch 2G 3.1.3", &e) == LT_OK);
    assert(lt_library_rename(l, first.id, "", &e) == LT_INVALID);
    assert(lt_library_rename(l, "../escape", "x", &e) == LT_INVALID);
    lt_record_info renamed{sizeof(renamed), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
    for (uint32_t i = 0; i < count; ++i) {
        assert(lt_library_record_at(l, i, &renamed, &e) == LT_OK);
        if (std::string(renamed.id) == first.id)
            break;
    }
    assert(std::string(renamed.display_name) == "iPod touch 2G 3.1.3" && std::string(renamed.base_sha256) == first.base_sha256);
    // Names round-trip through strict JSON: real UTF-8 (an emoji) is kept; JNI modified UTF-8 (the
    // emoji as two encoded surrogates), overlong forms and control characters are refused.
    assert(lt_library_rename(l, first.id, "Kai\xe2\x80\x99s iPod \xf0\x9f\x8e\xb5", &e) == LT_OK);
    assert(lt_library_rename(l, first.id, "x\xed\xa0\xbc\xed\xbe\xb5", &e) == LT_INVALID);
    assert(lt_library_rename(l, first.id, "\xc0\xaf", &e) == LT_INVALID);
    assert(lt_library_rename(l, first.id, "a\tb", &e) == LT_INVALID);
    assert(lt_library_rename(l, first.id, "\xe2\x80", &e) == LT_INVALID);
    for (uint32_t i = 0; i < count; ++i) {
        assert(lt_library_record_at(l, i, &renamed, &e) == LT_OK);
        if (std::string(renamed.id) == first.id)
            break;
    }
    assert(std::string(renamed.display_name) == "Kai\xe2\x80\x99s iPod \xf0\x9f\x8e\xb5");
    assert(lt_library_device_open(l, second_record.id, 0, &d, &e) == LT_OK);
    assert(lt_library_remove(l, second_record.id, &e) == LT_BUSY);
    lt_device_close(d);
    assert(lt_library_remove(l, second_record.id, &e) == LT_OK);
    assert(!fs::exists(second_record.state_path));
    assert(lt_library_count(l, &count, &e) == LT_OK && count == 1);
    assert(lt_library_selected(l, &selected_record, &e) == LT_OK && std::string(selected_record.id) == first.id);
    assert(lt_library_remove(l, second_record.id, &e) == LT_INVALID);
    lt_library_close(l);
    // A one-time migration retains stopped legacy user state and dirty marker,
    // rather than replacing the baseline when a second board is introduced.
    libroot = (root / "migrated-library").string();
    lc.root_path = libroot.c_str();
    assert(lt_library_open(&lc, &l, &e) == LT_OK);
    li.source_path = dest.c_str();
    li.display_name = "Legacy baseline";
    li.bootrom_path = rom.c_str();
    li.guest_package_path = nullptr;
    put(root / "state/nand/retained-user-page", "guest user data");
    assert(lt_library_adopt_legacy(l, &li, state.c_str(), &first, &e) == LT_OK);
    assert(first.dirty == 1 && fs::exists(fs::path(first.state_path) / "nand/retained-user-page"));
    assert(fs::exists(root / "state/nand/retained-user-page") && fs::exists(root / "state/.dirty"));
    assert(lt_library_device_open(l, first.id, 0, &d, &e) == LT_DIRTY);
    assert(lt_library_device_open(l, first.id, 1, &d, &e) == LT_OK);
    lt_device_close(d);
    lt_library_close(l);

    // A modified page invalidates the lock's full listing even when the lock file
    // itself still matches the device pin. Recover never silently accepts mutation.
    chmod((root / "base/nand/cs0/0.page").c_str(), 0600);
    put(root / "base/nand/cs0/0.page", "BAD!");
    chmod((root / "base/nand/cs0/0.page").c_str(), 0400);
    dc.base_path = (root / "base").c_str();
    dest = (root / "base").string();
    dc.base_path = dest.c_str();
    assert(lt_device_open(&dc, &d, &e) == LT_INVALID);
    for (const auto &entry : fs::recursive_directory_iterator(root))
        if (entry.is_directory())
            chmod(entry.path().c_str(), 0700);
    fs::remove_all(root);
    std::cout << "synthetic storage/ABI contracts passed (no guest boot claim)\n";
}
