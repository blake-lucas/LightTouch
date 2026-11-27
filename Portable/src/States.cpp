// Save states: a running device's whole machine and its storage at one moment, kept with its record.
//
// records/<id>/states/<state id>/ holds state.json (the manifest), thumbnail.png (the screen when it was saved) and
// state.zip (Zip.hpp): machine.state, the engine's stream of RAM and every device including the GL state, then the
// guest's storage as it was when the machine stopped for the save: state/nand/* (the NAND overlay), state/nor.bin,
// state/sim and state/guest-verdicts. A state is written as states/.<id>.partial and renamed into place complete.
//
// A state loads into a fresh engine process only (one start per process): its storage is put in place of the device's
// before the engine opens it, and the engine, started stopped (-S), loads the machine before its first instruction.
// It must match the device it loads into: the same prepared base (base_sha256), the same guest package (it lives in the
// guest's storage, and the host judges it), the same engine build (device state has no stable format across builds)
// and the same kind of host (the GL state is the host GL backend's). Refusals say so in a sentence.
#include "States.hpp"
#include "Records.hpp"
#include "Os.hpp"
#include <zlib.h>
#include <algorithm>
#include <chrono>
#include <ctime>
#include <fcntl.h>

namespace lt::states {
namespace {
constexpr int64_t schema = 1;
constexpr const char *manifest_name = "state.json", *thumbnail_name = "thumbnail.png", *archive_name = "state.zip";
// The guest's storage a state carries, beside state/nand's files.
const char *const storage_files[] = {"nor.bin", "sim", "guest-verdicts"};

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

int64_t now_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch())
        .count();
}

int64_t steady_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

void state_id(const std::string &id) {
    require(id.size() == 32 && id.find_first_not_of("0123456789abcdef") == std::string::npos, "invalid state ID");
}

void text(char *out, size_t capacity, const std::string &s) {
    const size_t n = std::min(s.size(), capacity - 1);
    memcpy(out, s.data(), n);
    out[n] = 0;
}

std::string default_name() {
    std::time_t now = std::time(nullptr);
    std::tm t{};
#ifdef _WIN32
    localtime_s(&t, &now);
#else
    localtime_r(&now, &t);
#endif
    char out[64];
    std::strftime(out, sizeof out, "%Y-%m-%d %H:%M:%S", &t);
    return out;
}

// A PNG of the picture, scaled down by a whole factor (averaging) until it is at most 480 pixels tall or wide.
std::string png(const Picture &p, uint32_t &width, uint32_t &height) {
    uint32_t factor = 1;
    while (std::max(p.width, p.height) / factor > 480)
        factor++;
    width = p.width / factor;
    height = p.height / factor;
    std::vector<uint8_t> raw;
    raw.reserve(size_t(height) * (size_t(width) * 3 + 1));
    for (uint32_t y = 0; y < height; ++y) {
        raw.push_back(0); // no filter
        for (uint32_t x = 0; x < width; ++x) {
            uint32_t sum[3] = {0, 0, 0};
            for (uint32_t dy = 0; dy < factor; ++dy)
                for (uint32_t dx = 0; dx < factor; ++dx) {
                    const uint8_t *px = &p.bgra[(size_t(y * factor + dy) * p.width + x * factor + dx) * 4];
                    sum[0] += px[2], sum[1] += px[1], sum[2] += px[0];
                }
            for (uint32_t c = 0; c < 3; ++c)
                raw.push_back(uint8_t(sum[c] / (factor * factor)));
        }
    }
    uLongf size = compressBound(uLong(raw.size()));
    std::vector<uint8_t> packed(size);
    require(compress2(packed.data(), &size, raw.data(), uLong(raw.size()), 6) == Z_OK, "thumbnail", LT_IO);
    packed.resize(size);
    std::string out("\x89PNG\r\n\x1a\n", 8);
    auto chunk = [&](const char *type, const std::vector<uint8_t> &data) {
        auto be32 = [&](uint32_t v) {
            for (int s = 24; s >= 0; s -= 8)
                out.push_back(char(v >> s));
        };
        be32(uint32_t(data.size()));
        const size_t start = out.size();
        out.append(type, 4);
        out.append(reinterpret_cast<const char *>(data.data()), data.size());
        be32(uint32_t(crc32(0, reinterpret_cast<const Bytef *>(out.data() + start), uInt(out.size() - start))));
    };
    std::vector<uint8_t> header;
    for (uint32_t v : {width, height})
        for (int s = 24; s >= 0; s -= 8)
            header.push_back(uint8_t(v >> s));
    header.insert(header.end(), {8, 2, 0, 0, 0}); // 8-bit RGB
    chunk("IHDR", header);
    chunk("IDAT", packed);
    chunk("IEND", {});
    return out;
}

void write_file(const fs::path &path, const std::string &bytes) {
    Fd fd(lt::os::open(path.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600));
    io(fd.n >= 0, "create " + path.filename().string());
    for (size_t done = 0; done < bytes.size();) {
        ssize_t k = lt::os::write(fd.n, bytes.data() + done, bytes.size() - done);
        if (k < 0 && errno == EINTR)
            continue;
        io(k > 0, "write " + path.filename().string());
        done += size_t(k);
    }
    io(lt::os::fsync(fd.n) == 0, "sync " + path.filename().string());
}

std::string json_text(const std::string &s) { return quote(s); }

// The storage files a save takes: state/nand's regular files (the overlay), then the others that exist.
std::vector<std::pair<std::string, fs::path>> storage(const lt_device *d) {
    std::vector<std::pair<std::string, fs::path>> files;
    const fs::path nand = d->state / "nand";
    if (fs::is_directory(fs::symlink_status(nand))) {
        std::vector<fs::path> names;
        for (const auto &e : fs::directory_iterator(nand)) {
            const std::string name = e.path().filename().string();
            if (name.empty() || name[0] == '.')
                continue;
            require(fs::is_regular_file(fs::symlink_status(e.path())),
                    "This device's storage is in an older format that save states can't hold", LT_UNSUPPORTED);
            names.push_back(e.path());
        }
        std::sort(names.begin(), names.end());
        for (const auto &p : names)
            files.emplace_back("state/nand/" + p.filename().string(), p);
    }
    for (const char *name : storage_files)
        if (fs::is_regular_file(fs::symlink_status(d->state / name)))
            files.emplace_back(std::string("state/") + name, d->state / name);
    return files;
}

void describe(const fs::path &folder, const Json &m, lt_state_info *out) {
    *out = lt_state_info{sizeof(lt_state_info), LT_ABI_VERSION, {}, {}, {}, 0, 0, 0, 0, {}};
    text(out->id, sizeof out->id, m.at("id").string());
    text(out->name, sizeof out->name, m.at("name").string());
    if (const auto *app = m.find("app"); app && app->kind == Json::String)
        text(out->app, sizeof out->app, app->string());
    out->created_ms = int64_t(m.at("created").integer());
    std::error_code ec;
    for (const char *name : {manifest_name, thumbnail_name, archive_name}) {
        const auto size = fs::file_size(folder / name, ec);
        if (!ec)
            out->bytes += size;
    }
    if (fs::is_regular_file(folder / thumbnail_name, ec)) {
        text(out->thumbnail_path, sizeof out->thumbnail_path, (folder / thumbnail_name).string());
        if (const auto *t = m.find("thumbnail"); t && t->kind == Json::Object) {
            out->thumbnail_width = uint32_t(t->at("width").integer());
            out->thumbnail_height = uint32_t(t->at("height").integer());
        }
    }
}

Json read_manifest(const fs::path &folder) {
    Json m = parse(read_file(folder / manifest_name, 1 << 16));
    require(m.kind == Json::Object && m.at("schema").integer() == uint64_t(schema), "unsupported save state");
    state_id(m.at("id").string());
    return m;
}

fs::path record_states(lt_library *l, const std::string &record) {
    record_id(record);
    const fs::path directory = l->root / "records" / record;
    require(fs::is_directory(fs::symlink_status(directory)), "no such device");
    return directory / "states";
}
} // namespace

const char *host_kind() {
#ifdef __ANDROID__
    return "android";
#else
    return "desktop";
#endif
}

fs::path directory(const lt_device *d) { return d->state.parent_path() / "states"; }

void Observer::begin(const char *name, uint64_t bytes) {
    phase = name;
    done = 0;
    total = bytes;
    report(true);
}
void Observer::add(uint64_t bytes) {
    done += bytes;
    if (done > total)
        total = done;
    report(false);
}
void Observer::report(bool force) {
    const int64_t now = steady_ms();
    if (!force && now - last_ms < 100)
        return;
    last_ms = now;
    if (fn && fn(opaque, phase.c_str(), done, total) != 0)
        throw Failure(LT_CANCELLED, "cancelled");
}

void save(lt_device *d, SaveRequest &request, Observer &observer, lt_state_info *out) {
    const std::string name = request.name.empty() ? default_name() : request.name;
    display_name(name);
    const auto files = storage(d);
    uint64_t storage_bytes = 0;
    for (const auto &f : files) {
        Fd in(lt::os::open(f.second.c_str(), O_RDONLY | O_CLOEXEC));
        io(in.n >= 0, "open " + f.first);
        lt::os::Stat st{};
        io(lt::os::fstat(in.n, &st) == 0, "inspect " + f.first);
        storage_bytes += uint64_t(st.st_size);
    }
    const fs::path states = directory(d);
    fs::create_directories(states);
    const std::string id = new_id();
    const fs::path partial = states / ("." + id + ".partial"), folder = states / id;
    fs::create_directories(partial);
    try {
        const int64_t created = now_ms();
        uint32_t thumb_width = 0, thumb_height = 0;
        if (request.picture.width && request.picture.height)
            write_file(partial / thumbnail_name, png(request.picture, thumb_width, thumb_height));
        observer.begin("saving", request.machine_estimate + storage_bytes);
        uint64_t machine_bytes = 0;
        {
            Fd zip_fd(lt::os::open((partial / archive_name).c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600));
            io(zip_fd.n >= 0, "create the save state");
            zip::Writer zip(zip_fd.n, [&](uint64_t n) { observer.add(n); });
            bool resumed = false;
            auto resume = [&] {
                if (!resumed) {
                    resumed = true;
                    request.resume();
                }
            };
            try {
                zip.begin("machine.state");
                request.machine([&](const void *p, size_t n) {
                    zip.write(p, n);
                    machine_bytes += n;
                });
                zip.end();
                // The guest is stopped: its storage agrees with the machine just written.
                for (const auto &f : files)
                    zip.file(f.first, f.second);
                // Every chunk read before the guest may write again.
                zip.finish();
                resume();
            } catch (...) {
                resume();
                throw;
            }
            io(lt::os::fsync(zip_fd.n) == 0, "sync the save state");
        }
        std::string m = "{\"schema\":" + std::to_string(schema) + ",\"id\":" + json_text(id) +
                        ",\"name\":" + json_text(name) + ",\"created\":" + std::to_string(created) +
                        ",\"app\":" + json_text(request.app) + ",\"engine\":" + json_text(request.engine) +
                        ",\"host\":" + json_text(host_kind()) + ",\"board\":" + json_text(d->board) +
                        ",\"build\":" + json_text(d->lock.at("build").string()) +
                        ",\"base_sha256\":" + json_text(d->base_digest) +
                        ",\"guest_package_sha256\":" + json_text(request.guest_package_sha256) +
                        ",\"headphones\":" + std::string(request.headphones ? "true" : "false") +
                        ",\"machine_bytes\":" + std::to_string(machine_bytes) +
                        ",\"storage_bytes\":" + std::to_string(storage_bytes);
        if (thumb_width)
            m += ",\"thumbnail\":{\"width\":" + std::to_string(thumb_width) +
                 ",\"height\":" + std::to_string(thumb_height) + "}";
        m += "}\n";
        write_file(partial / manifest_name, m);
        sync_dir(partial);
        io(lt::os::rename(partial.c_str(), folder.c_str()) == 0, "publish the save state");
        sync_dir(states);
        describe(folder, read_manifest(folder), out);
    } catch (...) {
        discard(partial);
        throw;
    }
}

Load::~Load() {
    reader.reset();
    if (fd >= 0)
        lt::os::close(fd);
}

std::unique_ptr<Load> open(lt_device *d, const std::string &id, const std::string &engine,
                           const std::string &guest_package_sha256) {
    state_id(id);
    const fs::path folder = directory(d) / id;
    require(fs::is_directory(fs::symlink_status(folder)), "That save state no longer exists");
    auto load = std::make_unique<Load>();
    load->id = id;
    load->manifest = read_manifest(folder);
    const Json &m = load->manifest;
    require(m.at("id").string() == id, "That save state is damaged");
    require(m.at("board").string() == d->board && m.at("base_sha256").string() == d->base_digest,
            "That save state belongs to another device");
    require(m.at("host").string() == host_kind(), std::string("That save state was made on ") +
                                                      (m.at("host").string() == "android" ? "Android" : "a computer") +
                                                      " and loads only there");
    require(m.at("engine").string() == engine, "That save state was made with another version of LightTouch (" +
                                                   m.at("app").string() + ") and can't be loaded by this one");
    require(m.at("guest_package_sha256").string() == guest_package_sha256,
            "This device's guest tools changed after that save state was made, so it can't be loaded");
    if (const auto *h = m.find("headphones"))
        load->headphones = h->kind == Json::Boolean && h->text == "true";
    load->fd = lt::os::open((folder / archive_name).c_str(), O_RDONLY | O_CLOEXEC);
    io(load->fd >= 0, "open the save state");
    load->reader = std::make_unique<zip::Reader>(load->fd);
    require(load->reader->find("machine.state"), "This save state is damaged or incomplete", LT_IO);
    return load;
}

void restore_storage(Load &load, lt_device *d, Observer &observer) {
    std::vector<const zip::Entry *> entries;
    uint64_t total = 0;
    for (const auto &e : load.reader->entries()) {
        if (e.name.rfind("state/", 0) != 0)
            continue;
        const std::string relative = e.name.substr(6);
        const bool known = (relative.rfind("nand/", 0) == 0 && relative.find('/', 5) == std::string::npos &&
                            relative.size() > 5 && relative[5] != '.') ||
                           std::find_if(std::begin(storage_files), std::end(storage_files),
                                        [&](const char *n) { return relative == n; }) != std::end(storage_files);
        require(known, "This save state is damaged or incomplete", LT_IO);
        entries.push_back(&e);
        total += e.size;
    }
    const fs::path stage = d->state / ".state-load";
    discard(stage);
    fs::create_directories(stage / "nand");
    try {
        observer.begin("restoring", total);
        for (const auto *e : entries)
            load.reader->extract(*e, stage / e->name.substr(6), [&](uint64_t n) { observer.add(n); });
        sync_dir(stage / "nand");
        sync_dir(stage);
        // Swap in: the old overlay goes whole (a file the state lacks would belong to other RAM).
        const fs::path nand = d->state / "nand", old = d->state / ".nand-replaced";
        discard(old);
        if (fs::exists(fs::symlink_status(nand)))
            io(lt::os::rename(nand.c_str(), old.c_str()) == 0, "set aside the device's storage");
        io(lt::os::rename((stage / "nand").c_str(), nand.c_str()) == 0, "restore the saved storage");
        for (const char *name : storage_files) {
            const fs::path from = stage / name, to = d->state / name;
            if (fs::exists(fs::symlink_status(from)))
                io(lt::os::rename(from.c_str(), to.c_str()) == 0, std::string("restore ") + name);
            else if (fs::exists(fs::symlink_status(to)))
                fs::remove(to);
        }
        sync_dir(d->state);
        discard(old);
        discard(stage);
    } catch (...) {
        discard(stage);
        throw;
    }
}

void read_machine(Load &load, const std::function<void(const uint8_t *, size_t)> &sink, Observer &observer) {
    const auto *e = load.reader->find("machine.state");
    observer.begin("loading", e->size);
    load.reader->read(*e, sink, [&](uint64_t n) { observer.add(n); });
}

} // namespace lt::states

extern "C" {
lt_status lt_library_states(lt_library *l, const char *record, lt_state_info *out, uint32_t capacity, uint32_t *count,
                            lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(record && count && (out || !capacity), "states output missing");
        lt::LibraryLock lock(l);
        const auto states = lt::states::record_states(l, record);
        std::vector<std::pair<lt::Json, lt::fs::path>> found;
        if (lt::fs::is_directory(lt::fs::symlink_status(states)))
            for (const auto &entry : lt::fs::directory_iterator(states)) {
                const std::string name = entry.path().filename().string();
                if (name.empty() || name[0] == '.')
                    continue;
                try {
                    found.emplace_back(lt::states::read_manifest(entry.path()), entry.path());
                } catch (const std::exception &) {
                    // A folder that isn't a complete state (removed by hand, another version's) isn't listed.
                }
            }
        std::sort(found.begin(), found.end(), [](const auto &a, const auto &b) {
            return a.first.at("created").integer() > b.first.at("created").integer();
        });
        *count = uint32_t(found.size());
        for (uint32_t i = 0; i < capacity && i < found.size(); ++i) {
            lt::abi(&out[i]);
            lt::states::describe(found[i].second, found[i].first, &out[i]);
        }
    });
}
lt_status lt_library_rename_state(lt_library *l, const char *record, const char *state, const char *name, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(record && state && name, "state name missing");
        lt::display_name(name);
        lt::LibraryLock lock(l);
        lt::states::state_id(state);
        const auto folder = lt::states::record_states(l, record) / state;
        auto manifest = lt::states::read_manifest(folder);
        manifest.object["name"].kind = lt::Json::String;
        manifest.object["name"].text = name;
        lt::atomic_write(folder / lt::states::manifest_name, lt::dump(manifest) + "\n");
    });
}
lt_status lt_library_delete_state(lt_library *l, const char *record, const char *state, lt_error *e) {
    return lt::guard(e, [&] {
        lt::require(record && state, "state ID missing");
        lt::LibraryLock lock(l);
        lt::states::state_id(state);
        const auto states = lt::states::record_states(l, record);
        const auto folder = states / state;
        lt::require(lt::fs::is_directory(lt::fs::symlink_status(folder)), "That save state no longer exists");
        // Renamed away first, so a half-deleted state is never listed.
        const auto doomed = states / ("." + std::string(state) + ".deleting");
        lt::discard(doomed);
        lt::io(lt::os::rename(folder.c_str(), doomed.c_str()) == 0, "delete the save state");
        lt::discard(doomed);
    });
}
}
