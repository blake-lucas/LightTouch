#pragma once
// The device library's record helpers (Library.cpp), shared with device transfer (Transfer.cpp).
#include "Internal.hpp"
struct lt_library {
    lt::fs::path root;
};
namespace lt {
// The library's mutation lock (records/, selected, order): held briefly, never across a long copy.
struct LibraryLock {
    int fd = -1;
    explicit LibraryLock(lt_library *l);
    ~LibraryLock();
    LibraryLock(const LibraryLock &) = delete;
    LibraryLock &operator=(const LibraryLock &) = delete;
};
std::string quote(const std::string &);
void record_id(const std::string &);
void display_name(const std::string &);
void discard(const fs::path &);
std::string new_id();
// Every record's ID, in the user's order (`.order`), new ones last.
std::vector<std::string> records(lt_library *);
// Write `.order`; and append a record just moved into records/ to the order taken before it arrived.
void save_order(lt_library *, const std::vector<std::string> &);
void add_to_order(lt_library *, std::vector<std::string> before, const std::string &id);
std::string selected(lt_library *);
void describe(lt_library *, const std::string &id, lt_record_info *);
// record.json for [metadata] (another record's) under a new id and name.
std::string record_json(const Json &metadata, const std::string &id, const std::string &name);
// Finish or apply the v0.0.13 kernel repair of a stopped record (the caller holds its lease).
void repair_kernel(const fs::path &directory, const std::string &id);
}  // namespace lt
