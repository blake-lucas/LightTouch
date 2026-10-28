#include "Internal.hpp"
#include <algorithm>
#include <array>
#include "Os.hpp"
#include <memory>
namespace lt {
static std::string quote(const std::string &s) {
    std::string out = "\"";
    static const char h[] = "0123456789abcdef";
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') {
            out += '\\';
            out += char(c);
        } else if (c < 32) {
            out += "\\u00";
            out += h[c >> 4];
            out += h[c & 15];
        } else
            out += char(c);
    }
    return out + '"';
}
static void record_id(const std::string &id) {
    require(!id.empty() && id.size() < 64 &&
                id.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-") ==
                    std::string::npos,
            "invalid record ID");
}
static void text(char *out, size_t capacity, const std::string &s) {
    require(s.size() < capacity, "record field exceeds ABI bound");
    memcpy(out, s.c_str(), s.size() + 1);
}
// Names are stored in record.json, which the JSON reader parses strictly: anything but well-formed
// UTF-8 (overlong forms, surrogates such as JNI's modified UTF-8 emits) would make the record unreadable.
static void display_name(const std::string &name) {
    require(!name.empty() && name.size() < 128, "device name must be 1-127 bytes");
    for (size_t i = 0; i < name.size();) {
        auto c = static_cast<unsigned char>(name[i]);
        require(c >= 0x20 && c != 0x7f, "device name has control characters");
        size_t n = c < 0x80 ? 1 : (c & 0xe0) == 0xc0 ? 2 : (c & 0xf0) == 0xe0 ? 3 : (c & 0xf8) == 0xf0 ? 4 : 0;
        require(n && i + n <= name.size(), "device name is not valid UTF-8");
        uint32_t code = n == 1 ? c : c & (0x7f >> n);
        for (size_t k = 1; k < n; ++k) {
            auto b = static_cast<unsigned char>(name[i + k]);
            require((b & 0xc0) == 0x80, "device name is not valid UTF-8");
            code = code << 6 | (b & 0x3f);
        }
        static const uint32_t least[] = {0, 0, 0x80, 0x800, 0x10000};
        require(code >= least[n] && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff), "device name is not valid UTF-8");
        i += n;
    }
}
// The copy is the record's own file: it never takes the source's mode (a read-only ROM stays deletable with its
// record).
static void copy_regular(const fs::path &source, const fs::path &destination) {
    require(fs::is_regular_file(fs::symlink_status(source)), "record asset must be regular file");
    copy_contents(source, destination);
}
static void copy_state(const fs::path &source, const fs::path &destination) {
    fs::create_directories(destination);
    for (const auto &e : fs::recursive_directory_iterator(source)) {
        auto relative = e.path().lexically_relative(source);
        if (relative == ".lease")
            continue;
        require(!e.is_symlink(), "legacy state symlink refused");
        auto target = destination / relative;
        if (e.is_directory())
            fs::create_directories(target);
        else {
            require(e.is_regular_file(), "legacy state special file refused");
            fs::create_directories(target.parent_path());
            copy_regular(e.path(), target);
        }
    }
    for (const auto &e : fs::recursive_directory_iterator(destination))
        if (e.is_directory())
            sync_dir(e.path());
    sync_dir(destination);
}
static void discard(const fs::path &p) {
    std::error_code error;
    if (fs::remove_all(p, error) != static_cast<std::uintmax_t>(-1) && !error)
        return;
    // Records made before NAND containers froze their base and assets read-only (0500 folders), which an unlink
    // inside them needs undone. Nothing made since is read-only. Files too: on Windows only a file's read-only
    // attribute blocks its delete.
    for (const auto &e : fs::recursive_directory_iterator(p, error))
        lt::os::chmod(e.path().c_str(), 0700);
    lt::os::chmod(p.c_str(), 0700);
    fs::remove_all(p, error);
}
static std::string new_id() {
    std::array<uint8_t, 16> b{};
    io(lt::os::random_bytes(b.data(), b.size()) == 0, "record identity entropy");
    static const char hex[] = "0123456789abcdef";
    std::string id;
    for (auto c : b) {
        id += hex[c >> 4];
        id += hex[c & 15];
    }
    return id;
}
} // namespace lt
struct lt_library {
    lt::fs::path root;
};
namespace lt {
struct LibraryLock {
    int fd = -1;
    explicit LibraryLock(lt_library *l) {
        require(l, "null library");
        fd = lt::os::open((l->root / ".library-lock").c_str(), O_RDWR | O_CREAT | O_NOFOLLOW | O_CLOEXEC, 0600);
        io(fd >= 0, "library mutation lock");
        if (lt::os::lock_exclusive(fd) != 0) {
            lt::os::close(fd);
            fd = -1;
            throw Failure(LT_BUSY, "device library operation in progress");
        }
    }
    ~LibraryLock() {
        if (fd >= 0)
            lt::os::close(fd);
    }
};
static std::vector<std::string> records(lt_library *l) {
    std::vector<std::string> ids;
    for (const auto &e : fs::directory_iterator(l->root / "records")) {
        auto id = e.path().filename().string();
        if (id.empty() || id[0] == '.')
            continue;
        record_id(id);
        require(e.is_directory() && !e.is_symlink(), "invalid record storage");
        require(fs::is_regular_file(fs::symlink_status(e.path() / "record.json")), "incomplete device record");
        ids.push_back(id);
    }
    std::sort(ids.begin(), ids.end());
    return ids;
}
static std::string selected(lt_library *l) {
    if (!fs::exists(l->root / "selected"))
        return "";
    std::string id = read_file(l->root / "selected", 64);
    record_id(id);
    return id;
}
static void describe(lt_library *l, const std::string &id, lt_record_info *out) {
    abi(out);
    record_id(id);
    auto directory = l->root / "records" / id;
    auto j = parse(read_file(directory / "record.json", 65536));
    require(j.at("schema").integer() == 1 && j.at("id").string() == id, "invalid device record schema/identity");
    text(out->id, sizeof out->id, id);
    text(out->display_name, sizeof out->display_name, j.at("name").string());
    text(out->board, sizeof out->board, j.at("board").string());
    text(out->build, sizeof out->build, j.at("build").string());
    text(out->base_sha256, sizeof out->base_sha256, j.at("base_sha256").string());
    text(out->base_path, sizeof out->base_path, (directory / "base").string());
    text(out->state_path, sizeof out->state_path, (directory / "state").string());
    text(out->bootrom_path, sizeof out->bootrom_path,
         fs::exists(directory / "assets/bootrom.bin") ? (directory / "assets/bootrom.bin").string() : "");
    text(out->guest_package_path, sizeof out->guest_package_path,
         fs::exists(directory / "assets/guest.itpack") ? (directory / "assets/guest.itpack").string() : "");
    out->selected = selected(l) == id;
    out->dirty = fs::exists(directory / "state/.dirty");
}
static void import(lt_library *l, const lt_library_import_config *c, const char *legacy_state, lt_record_info *out,
                   lt_import_phase_fn observer = nullptr, void *opaque = nullptr, bool adopt = false) {
    abi(c);
    abi(out);
    require(c->source_path, "record source missing");
    std::unique_ptr<lt_device, decltype(&lt_device_close)> legacy(nullptr, lt_device_close);
    if (legacy_state) {
        require(records(l).empty(), "legacy adoption only into empty library", LT_BUSY);
        lt_device_config old{sizeof(old), 1, c->source_path, legacy_state, 1};
        lt_device *d = nullptr;
        lt_error e{sizeof(e), 1, 0, {}};
        auto status = lt_device_open(&old, &d, &e);
        require(status == LT_OK, e.message, status);
        legacy.reset(d);
    }
    std::string id = new_id();
    std::string pattern = (l->root / "records/.record-stage-XXXXXX").string();
    std::vector<char> tmp(pattern.begin(), pattern.end());
    tmp.push_back(0);
    io(lt::os::mkdtemp(tmp.data()) != nullptr, "record staging");
    fs::path stage = tmp.data();
    fs::path adopted;  // the caller's folder while it is ours, to hand back if the import fails
    try {
        std::string base = (stage / "base").string();
        if (adopt) {
            require(fs::is_directory(fs::symlink_status(c->source_path)), "prepared output must be a folder");
            lt::os::Stat from {}, to {};
            io(lt::os::stat(c->source_path, &from) == 0 && lt::os::stat(l->root.c_str(), &to) == 0, "stat prepared output");
            require(from.st_dev == to.st_dev, "prepared output must be on the library's storage");
            adopted = fs::canonical(c->source_path);
            io(lt::os::rename(adopted.c_str(), base.c_str()) == 0, "move prepared output");
            adopt_base(base, observer, opaque);
        } else {
            lt_import_config cfg{sizeof(cfg),  1,           c->source_path, base.c_str(), c->expected_sha256,
                                 c->max_bytes, c->progress, c->opaque};
            lt_error e{sizeof(e), 1, 0, {}};
            lt_status status = fs::is_directory(fs::symlink_status(c->source_path))
                                   ? lt_import_directory_observed(&cfg, observer, opaque, &e)
                                   : lt_import_archive_observed(&cfg, observer, opaque, &e);
            require(status == LT_OK, e.message, status);
        }
        auto lock = parse(read_file(stage / "base/device.lock.json"));
        std::string board = lock.at("board").string(), build = lock.at("build").string(),
                    name = c->display_name && *c->display_name ? c->display_name : board + " " + build;
        display_name(name);
        fs::create_directory(stage / "assets");
        if (c->bootrom_path && *c->bootrom_path)
            copy_regular(c->bootrom_path, stage / "assets/bootrom.bin");
        if (c->guest_package_path && *c->guest_package_path)
            copy_regular(c->guest_package_path, stage / "assets/guest.itpack");
        else if (const auto *outputs = lock.find("outputs"))
            if (const auto *package = outputs->find("guest_package")) {
                auto source = stage / "base" / package->at("path").string();
                require(digest_file(source) == package->at("sha256").string(),
                        "prepared guest package checksum mismatch");
                copy_regular(source, stage / "assets/guest.itpack");
            }
        if (legacy_state)
            copy_state(legacy->state, stage / "state");
        else
            fs::create_directory(stage / "state");
        std::string metadata = "{\"schema\":1,\"id\":" + quote(id) + ",\"name\":" + quote(name) +
                               ",\"board\":" + quote(board) + ",\"build\":" + quote(build) +
                               ",\"base_sha256\":" + quote(digest_file(stage / "base/device.lock.json"));
        if (fs::exists(stage / "assets/bootrom.bin"))
            metadata += ",\"bootrom_sha256\":" + quote(digest_file(stage / "assets/bootrom.bin"));
        if (fs::exists(stage / "assets/guest.itpack"))
            metadata += ",\"guest_package_sha256\":" + quote(digest_file(stage / "assets/guest.itpack"));
        metadata += "}\n";
        atomic_write(stage / "record.json", metadata);
        sync_dir(stage / "assets");
        sync_dir(stage / "state");
        sync_dir(stage);
        fs::rename(stage, l->root / "records" / id);
        sync_dir(l->root / "records");
        if (adopt) {
            // Best effort: without it the first start reads the base once, as after any import.
            try {
                seed_admission(l->root / "records" / id / "base", l->root / "records" / id / "state");
            } catch (const std::exception &) {
            }
        }
        if (selected(l).empty())
            atomic_write(l->root / "selected", id);
        describe(l, id, out);
    } catch (...) {
        std::error_code ec;
        if (!adopted.empty() && fs::is_directory(stage / "base", ec) && !fs::exists(adopted, ec))
            fs::rename(stage / "base", adopted, ec);
        discard(stage);
        throw;
    }
}
/* record.json with one field replaced; the schema has no nested values. */
static std::string record_json(const Json &metadata, const std::string &id, const std::string &name) {
    std::string json = "{\"schema\":1,\"id\":" + quote(id) + ",\"name\":" + quote(name) +
                       ",\"board\":" + quote(metadata.at("board").string()) +
                       ",\"build\":" + quote(metadata.at("build").string()) +
                       ",\"base_sha256\":" + quote(metadata.at("base_sha256").string());
    if (const auto *rom = metadata.find("bootrom_sha256"))
        json += ",\"bootrom_sha256\":" + quote(rom->string());
    if (const auto *package = metadata.find("guest_package_sha256"))
        json += ",\"guest_package_sha256\":" + quote(package->string());
    return json + "}\n";
}
static void rename_record(lt_library *l, const std::string &id, const std::string &name) {
    record_id(id);
    display_name(name);
    auto directory = l->root / "records" / id;
    auto metadata = parse(read_file(directory / "record.json", 65536));
    require(metadata.at("schema").integer() == 1 && metadata.at("id").string() == id, "invalid device record");
    atomic_write(directory / "record.json", record_json(metadata, id, name));
    sync_dir(directory);
}
static void remove_record(lt_library *l, const std::string &id) {
    record_id(id);
    auto directory = l->root / "records" / id;
    require(fs::is_directory(fs::symlink_status(directory)), "no such device", LT_INVALID);
    int lease = lt::os::open((directory / "state/.lease").c_str(), O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    io(lease >= 0, "device lease");
    struct Closer {
        int fd;
        ~Closer() {
            if (fd >= 0)
                lt::os::close(fd);
        }
    } closer{lease};
    require(lt::os::lock_exclusive(lease) == 0, "stop the device before deleting it", LT_BUSY);
#ifdef _WIN32
    // Windows won't rename a directory with a file open inside it, the lease included. The library lock, held
    // until the record is gone, still keeps the device from opening; a process with the record's files open
    // makes the rename fail instead.
    lt::os::close(lease);
    closer.fd = -1;
#endif
    // Trash left by a delete that crashed or failed before.
    std::vector<fs::path> stale;
    std::error_code ignored;
    for (const auto &e : fs::directory_iterator(l->root / "records", ignored))
        if (e.path().filename().string().rfind(".record-trash-", 0) == 0)
            stale.push_back(e.path());
    for (const auto &p : stale)
        discard(p);
    // Out of the record namespace first, so a crash mid-delete leaves no half record.
    std::string pattern = (l->root / "records/.record-trash-XXXXXX").string();
    std::vector<char> tmp(pattern.begin(), pattern.end());
    tmp.push_back(0);
    io(lt::os::mkdtemp(tmp.data()) != nullptr, "record trash");
    fs::path trash = tmp.data();
    std::error_code moved;
    fs::rename(directory, trash / "record", moved);
    if (moved) {
        discard(trash);
        require(false,
                "its files couldn't be moved (" + moved.message() +
                    "); if another program has them open, close it and try again",
                LT_IO);
    }
    sync_dir(l->root / "records");
    if (selected(l) == id) {
        auto rest = records(l);
        if (rest.empty())
            fs::remove(l->root / "selected");
        else
            atomic_write(l->root / "selected", rest.front());
    }
    discard(trash);
}
static void set_guest_package(lt_library *l, const std::string &id, const char *path, lt_record_info *out) {
    abi(out);
    record_id(id);
    require(path && *path, "guest package path missing");
    auto directory = l->root / "records" / id;
    auto metadata = parse(read_file(directory / "record.json", 65536));
    require(metadata.at("schema").integer() == 1 && metadata.at("id").string() == id, "invalid device record");
    // The device's own lease: a running (or opening) device holds it.
    int lease = lt::os::open((directory / "state/.lease").c_str(), O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW, 0600);
    io(lease >= 0, "device lease");
    struct Closer {
        int fd;
        ~Closer() { lt::os::close(fd); }
    } closer{lease};
    require(lt::os::lock_exclusive(lease) == 0, "stop the device before updating its guest tools", LT_BUSY);
    require(fs::is_regular_file(fs::symlink_status(path)), "guest package must be a regular .itpack file");
    {
        char magic[8] = {};
        int fd = lt::os::open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        io(fd >= 0, "open guest package");
        ssize_t n = lt::os::read(fd, magic, sizeof magic);
        lt::os::close(fd);
        require(n == 8 && std::memcmp(magic, "ITPACK01", 8) == 0, "not a guest tools package (ITPACK01)");
    }
    auto assets = directory / "assets";
    fs::create_directories(assets);
    // A record made before NAND containers froze its assets read-only (0500); writing into them needs that undone.
    if (lt::os::read_only(assets.c_str()))
        io(lt::os::chmod(assets.c_str(), 0700) == 0, "record assets permissions");
    try {
        std::error_code stale;
        fs::remove(assets / ".guest.itpack.new", stale);
        copy_regular(path, assets / ".guest.itpack.new");
        std::string sha = digest_file(assets / ".guest.itpack.new");
        fs::rename(assets / ".guest.itpack.new", assets / "guest.itpack");
        sync_dir(assets);
        std::string json = "{\"schema\":1,\"id\":" + quote(id) + ",\"name\":" + quote(metadata.at("name").string()) +
                           ",\"board\":" + quote(metadata.at("board").string()) +
                           ",\"build\":" + quote(metadata.at("build").string()) +
                           ",\"base_sha256\":" + quote(metadata.at("base_sha256").string());
        if (const auto *rom = metadata.find("bootrom_sha256"))
            json += ",\"bootrom_sha256\":" + quote(rom->string());
        json += ",\"guest_package_sha256\":" + quote(sha) + "}\n";
        atomic_write(directory / "record.json", json);
        sync_dir(directory);
    } catch (...) {
        std::error_code ec;
        fs::remove(assets / ".guest.itpack.new", ec);
        throw;
    }
    describe(l, id, out);
}
} // namespace lt
extern "C" {
lt_status lt_library_set_guest_package(lt_library *l, const char *id, const char *path, lt_record_info *out,
                                       lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(id, "record ID missing");
        lt::set_guest_package(l, id, path, out);
    });
}
lt_status lt_library_rename(lt_library *l, const char *id, const char *name, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(id && name, "record ID/name missing");
        lt::rename_record(l, id, name);
    });
}
lt_status lt_library_remove(lt_library *l, const char *id, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(id, "record ID missing");
        lt::remove_record(l, id);
    });
}
lt_status lt_library_open(const lt_library_config *c, lt_library **out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::abi(c);
        lt::require(out && c->root_path && lt::fs::path(c->root_path).is_absolute(),
                    "private library absolute root required");
        *out = nullptr;
        lt::fs::create_directories(c->root_path);
        lt::require(!lt::fs::is_symlink(lt::fs::symlink_status(c->root_path)), "library root symlink refused");
        auto l = std::make_unique<lt_library>();
        l->root = lt::fs::canonical(c->root_path);
        lt::require(l->root.string().size() < 900, "library path exceeds record ABI bound");
        lt::fs::create_directories(l->root / "records");
        lt::require(!lt::fs::is_symlink(lt::fs::symlink_status(l->root / "records")),
                    "library records symlink refused");
        *out = l.release();
    });
}
void lt_library_close(lt_library *l) { delete l; }
lt_status lt_library_count(lt_library *l, uint32_t *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(out, "record count output missing");
        *out = uint32_t(lt::records(l).size());
    });
}
lt_status lt_library_record_at(lt_library *l, uint32_t index, lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        auto ids = lt::records(l);
        lt::require(index < ids.size(), "record index outside library", LT_AGAIN);
        lt::describe(l, ids[index], out);
    });
}
lt_status lt_library_select(lt_library *l, const char *id, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(id, "record ID missing");
        lt_record_info info{sizeof(info), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
        lt::describe(l, id, &info);
        lt::atomic_write(l->root / "selected", id);
    });
}
lt_status lt_library_selected(lt_library *l, lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        auto id = lt::selected(l);
        lt::require(!id.empty(), "no device selected", LT_AGAIN);
        lt::describe(l, id, out);
    });
}
lt_status lt_library_import(lt_library *l, const lt_library_import_config *c, lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::import(l, c, nullptr, out);
    });
}
lt_status lt_library_import_observed(lt_library *l, const lt_library_import_config *c, lt_import_phase_fn observer,
                                     void *opaque, lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::import(l, c, nullptr, out, observer, opaque);
    });
}
lt_status lt_library_adopt_prepared(lt_library *l, const lt_library_import_config *c, lt_import_phase_fn observer,
                                    void *opaque, lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::import(l, c, nullptr, out, observer, opaque, true);
    });
}
lt_status lt_library_adopt_legacy(lt_library *l, const lt_library_import_config *c, const char *state,
                                  lt_record_info *out, lt_error *e) {
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(state, "legacy state missing");
        lt::import(l, c, state, out);
    });
}
lt_status lt_library_device_open(lt_library *l, const char *id, uint32_t recover, lt_device **out, lt_error *e) {
    return lt_library_device_open_observed(l, id, recover, nullptr, nullptr, out, e);
}
lt_status lt_library_device_open_observed(lt_library *l, const char *id, uint32_t recover, lt_progress_fn observer,
                                          void *opaque, lt_device **out, lt_error *e) {
    if (out)
        *out = nullptr;
    return lt::guard(e, [&] {
        lt::LibraryLock lock(l);
        lt::require(id && out, "device record ID/output missing");
        *out = nullptr;
        lt_record_info info{sizeof(info), 1, {}, {}, {}, {}, {}, {}, {}, {}, {}, 0, 0};
        lt::describe(l, id, &info);
        auto metadata = lt::parse(lt::read_file(l->root / "records" / id / "record.json", 65536));
        std::vector<lt::AdmissionAssetCheck> assets;
        if (*info.bootrom_path)
            assets.push_back(
                {info.bootrom_path, metadata.at("bootrom_sha256").string(), "record ROM checksum mismatch"});
        if (*info.guest_package_path)
            assets.push_back({info.guest_package_path, metadata.at("guest_package_sha256").string(),
                              "record guest package checksum mismatch"});
        lt_device_config cfg{sizeof(cfg), 1, info.base_path, info.state_path, recover};
        auto device = lt::open_device_checked(&cfg, observer, opaque, assets);
        lt::require(device->base_digest == info.base_sha256, "record base integrity pin mismatch");
        *out = device.release();
    });
}
}
