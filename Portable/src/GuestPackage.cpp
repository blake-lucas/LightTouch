#include "Internal.hpp"
#include "GuestPackageJudge.hpp"
#include <zlib.h>
#include "Os.hpp"
#include <set>
#include <algorithm>
namespace lt {
static void relative_name(const std::string &s) {
    require(!s.empty() && s.size() < 1024 && s.find_first_of("\n\r\t \\") == std::string::npos &&
                s.find('\0') == std::string::npos && s[0] != '/',
            "unsafe package name");
    size_t at = 0;
    for (;;) {
        size_t end = s.find('/', at);
        auto part = s.substr(at, end - at);
        require(part != "." && part != ".." && !part.empty(), "unsafe package component");
        if (end == std::string::npos)
            break;
        at = end + 1;
    }
}
static bool contains(const Json &array, const std::string &text) {
    require(array.kind == Json::Array, "invalid package requirement");
    for (const auto &v : array.array)
        if (v.string() == text)
            return true;
    return false;
}
void record_guest_verdict(lt_device *d, bool good, int64_t serial) {
    auto path = d->state / "guest-verdicts";
    std::error_code missing;
    auto verdicts = GuestVerdicts::parse(fs::exists(path, missing) ? read_file(path, 4096) : "");
    int64_t seed = -1;
    if (const auto *record = d->lock.find("guest_package"))
        if (const auto *s = record->find("seed"))
            seed = int64_t(s->integer());
    if (verdicts.judge(good ? GuestPackageJudge::Verdict::good : GuestPackageJudge::Verdict::bad, serial, seed))
        atomic_write(path, verdicts.text());
}
bool guest_agent_expected(lt_device *d) {
    auto path = d->state / "guest-verdicts";
    std::error_code missing;
    return fs::exists(path, missing) && GuestVerdicts::parse(read_file(path, 4096)).good >= 0;
}
std::string guest_offer(lt_device *d, const char *pack) {
    require(fs::is_regular_file(fs::symlink_status(pack)), "guest package must be a regular .itpack");
    auto blob = read_file(pack);
    require(blob.size() >= 12 && blob.compare(0, 8, "ITPACK01") == 0, "not a guest ITPACK01 archive");
    auto *b = reinterpret_cast<const uint8_t *>(blob.data());
    uint32_t length = uint32_t(b[8]) | (uint32_t(b[9]) << 8) | (uint32_t(b[10]) << 16) | (uint32_t(b[11]) << 24);
    require(length > 0 && length < 16u << 20 && blob.size() > 12ull + length, "invalid guest index");
    auto index = parse(blob.substr(12, length));
    const auto &entries = index.at("entries");
    require(entries.kind == Json::Array && entries.array.size() <= 10000, "guest entry bound");
    uint64_t total = 0;
    std::set<std::string> names;
    for (const auto &e : entries.array) {
        auto name = e.at("name").string();
        relative_name(name);
        require(names.insert(name).second, "duplicate guest entry");
        uint64_t n = e.at("size").integer();
        require(n <= 64u << 20 && total + n <= 64u << 20, "guest package exceeds memory limit");
        total += n;
    }
    std::string payload(size_t(total), '\0');
    z_stream z{};
    require(inflateInit(&z) == Z_OK, "guest inflate initialization");
    z.next_in = reinterpret_cast<Bytef *>(blob.data() + 12 + length);
    z.avail_in = uInt(blob.size() - 12 - length);
    z.next_out = reinterpret_cast<Bytef *>(payload.data());
    z.avail_out = uInt(payload.size());
    int result = inflate(&z, Z_FINISH);
    bool valid = result == Z_STREAM_END && z.total_out == total && z.avail_in == 0;
    inflateEnd(&z);
    require(valid, "guest compressed checksum/length invalid");
    std::map<std::string, std::pair<size_t, size_t>> parts;
    size_t offset = 0;
    for (const auto &e : entries.array) {
        size_t n = size_t(e.at("size").integer());
        parts.emplace(e.at("name").string(), std::make_pair(offset, n));
        offset += n;
    }
    std::string board = d->lock.at("board").string(), build = d->lock.at("build").string(), prefix;
    Json manifest;
    bool found = false;
    for (const auto &entry : parts) {
        const auto &name = entry.first;
        if (name.size() < 14 || name.compare(name.size() - 14, 14, "/manifest.json"))
            continue;
        auto m = parse(payload.substr(entry.second.first, entry.second.second));
        const auto &r = m.at("requires");
        if (!contains(r.at("boards"), board))
            continue;
        bool matches = false;
        std::string major = build.substr(0, build.find_first_not_of("0123456789"));
        for (const auto &v : r.at("builds").array) {
            auto s = v.string();
            matches = matches || s == build || s == major + "*";
        }
        if (!matches)
            continue;
        if (const auto *stub = m.find("stub"))
            if (stub->text == "true")
                continue;
        require(!found, "ambiguous guest package family");
        manifest = m;
        prefix = name.substr(0, name.size() - 13);
        found = true;
    }
    require(found, "guest package has no supported family for " + board + "-" + build, LT_UNSUPPORTED);
    require(manifest.at("format").integer() == 1, "unsupported guest manifest", LT_UNSUPPORTED);
    const auto &host = manifest.at("requires").at("host");
    const auto &range = host.at("guest-package");
    require(range.kind == Json::Array && range.array.size() == 2 && range.array[0].integer() <= 1 &&
                range.array[1].integer() >= 1,
            "unsupported guest package protocol", LT_UNSUPPORTED);
    std::set<std::string> allowed_hooks;
    bool filter_hooks = false, gles = false;
    if (const auto *record = d->lock.find("guest_package")) {
        if (const auto *g = record->find("gles"))
            gles = g->text == "true";
        if (const auto *h = record->find("hooks")) {
            filter_hooks = true;
            for (const auto &v : h->array)
                allowed_hooks.insert(v.string());
        }
    }
    std::map<std::string, Json> hooks;
    std::set<std::string> dropped;
    for (const auto &h : manifest.at("hooks").array) {
        auto file = h.at("file").string(), target = h.at("target").string();
        bool gl = target == "/System/Library/Frameworks/OpenGLES.framework/OpenGLES" ||
                  target == "/System/Library/Frameworks/OpenGLES.framework/MBXGLEngine.bundle/MBXGLEngine";
        if ((gl && !gles) || (filter_hooks && !allowed_hooks.count(target)))
            dropped.insert(file);
        else
            hooks.emplace(file, h);
    }
    fs::create_directories(d->state / "work");
    std::string pattern = (d->state / "work/.offer-stage-XXXXXX").string();
    std::vector<char> tmp(pattern.begin(), pattern.end());
    tmp.push_back(0);
    io(lt::os::mkdtemp(tmp.data()) != nullptr, "guest offer stage");
    fs::path stage = tmp.data();
    try {
        std::string offer = "ltpkg 1\nbuild " + build + "\nserial " + manifest.at("serial").text + " " +
                            manifest.at("version").string() + "\n";
        std::error_code missing;
        if (fs::exists(d->state / "guest-verdicts", missing))
            offer += GuestVerdicts::parse(read_file(d->state / "guest-verdicts", 4096)).text("verdict ");
        size_t index = 0;
        for (const auto &f : manifest.at("files").array) {
            auto name = f.at("name").string();
            relative_name(name);
            if (dropped.count(name))
                continue;
            auto part = parts.find(prefix + name);
            require(part != parts.end(), "missing package payload: " + name);
            size_t n = part->second.second;
            require(n == f.at("size").integer(), "guest payload size mismatch");
            Sha256 hash;
            hash.update(payload.data() + part->second.first, n);
            require(hash.finish() == f.at("sha256").string(), "guest payload checksum mismatch: " + name);
            auto path = stage / name;
            fs::create_directories(path.parent_path());
            atomic_write(path, payload.substr(part->second.first, n));
            std::string kind = hooks.count(name) ? "hook" : contains(manifest.at("jobs"), name) ? "job" : "file";
            auto mode = f.at("mode").string();
            require(mode.size() == 3 && mode.find_first_not_of("01234567") == std::string::npos,
                    "invalid package mode");
            std::string line = kind + " " + std::to_string(index++) + " " + name + " " + mode + " " +
                               f.at("size").text + " " + f.at("sha256").string();
            if (hooks.count(name)) {
                const auto &h = hooks.at(name);
                auto target = h.at("target").string();
                require(target.find_first_of(" \n\r\t") == std::string::npos, "invalid hook target");
                line += " " + target;
                if (h.at("respring").text == "true")
                    line += " respring";
            }
            offer += line + "\n";
        }
        atomic_write(stage / "offer", offer);
        sync_dir(stage);
        auto destination = d->state / "work/guest-offer";
        std::error_code ec;
        fs::remove_all(destination, ec);
        require(!ec, "remove old ephemeral offer: " + ec.message(), LT_IO);
        fs::rename(stage, destination);
        sync_dir(destination.parent_path());
        return destination.string();
    } catch (...) {
        std::error_code ec;
        fs::remove_all(stage, ec);
        throw;
    }
}
} // namespace lt
