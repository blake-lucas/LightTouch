#pragma once
// Repairs the sandbox patch in a jailbroken kboot device's kernel (kboot.bin).
//
// v0.0.13's preparer made the Seatbelt kext's sb_evaluate return "allowed" with its result struct cleared, action
// byte included. The execve hook needs the profile-switch flag in that byte before a sandboxed process may exec a
// program that has a built-in profile of its own: BTServer runs BlueTool, so on the iPod touch 3G and iPhone 3GS the
// hook killed BTServer at every start and launchd restarted it all session (Home screen at ~2/3 of the stock frame
// rate). The preparer now writes the flag (crates/lighttouch-firmware/src/sandbox_patch.rs); this finds devices
// prepared by v0.0.13 and puts today's patch in place of that one, so they need not be prepared again.
//
// kboot.bin holds the kernel as loaded: each segment at its virtual address less the kernel's base (the lowest
// address rounded down to 256 MiB), so an image offset is an address less that base. The locator follows
// sandbox_patch.rs: sb_evaluate is the function around the kext's reference to its "bad opcode" report, and the flag
// is the single-bit tst/ands next to the execve hook's "unable to switch" message. A patched sb_evaluate begins with
// a patch in place of its push prologue.
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <map>
#include <optional>
#include <string>
#include <vector>

namespace lt::kernel_repair {

// Today's patch: movs r1,#0; str r1,[r0]; movs r1,#FLAG; strb r1,[r0,#4]; bx lr (FLAG at index 4).
inline constexpr uint8_t patch_bytes[10] = {0x00, 0x21, 0x01, 0x60, 0x00, 0x21, 0x01, 0x71, 0x70, 0x47};
inline constexpr size_t flag_index = 4;
// v0.0.13's: movs r1,#0; str r1,[r0]; str r1,[r0,#4]; movs r0,#0; bx lr.
inline constexpr uint8_t old_patch_bytes[10] = {0x00, 0x21, 0x01, 0x60, 0x41, 0x60, 0x00, 0x20, 0x70, 0x47};

struct Site {
    enum Kind { stock, old_patch, patch };
    size_t offset = 0;       // sb_evaluate's first byte in the image
    Kind kind = stock;       // a stock push prologue there, or one of the patches
    uint8_t switch_bit = 0;  // the flag the execve hook tests
};

class Image {
  public:
    // `bytes`: a loaded kernel image (kboot.bin's) whose Mach-O header sits at `header`.
    Image(const uint8_t *bytes, size_t size, size_t header) : b_(bytes), n_(size) {
        if (header + 28 > n_ || u32(header) != 0xFEEDFACE || u32(header + 4) != 12)
            return;
        size_t off = header + 28;
        uint32_t lowest = 0xFFFFFFFF;
        for (uint32_t i = 0, cmds = u32(header + 16); i < cmds && off + 8 <= n_; i++) {
            uint32_t cmd = u32(off), size = u32(off + 4);
            if (size < 8 || off + size > n_)
                break;
            if (cmd == 1 && size >= 56) {  // LC_SEGMENT
                Segment s{name(off + 8), u32(off + 24), u32(off + 28), u32(off + 36)};
                if (s.vmaddr != 0)
                    lowest = std::min(lowest, s.vmaddr);
                segments_.push_back(s);
                size_t so = off + 56;
                for (uint32_t k = 0, nsects = u32(off + 48); k < nsects && so + 68 <= off + size; k++, so += 68)
                    sections_.push_back({name(so), name(so + 16), u32(so + 32), u32(so + 36)});
            }
            off += size;
        }
        if (lowest == 0xFFFFFFFF)
            return;
        base_ = lowest & 0xF0000000u;
        // The header must sit where its own segment loads it.
        for (const auto &s : segments_)
            if (s.name == "__TEXT" && s.vmaddr - base_ == header)
                valid_ = true;
    }
    bool valid() const { return valid_; }

    // The loaded image's offset of `addr`, inside a segment's file-backed bytes.
    std::optional<size_t> offset(uint32_t addr) const {
        for (const auto &s : segments_)
            if (addr >= s.vmaddr && addr - s.vmaddr < std::min(s.vmsize, s.filesize) && addr - base_ < n_)
                return size_t(addr - base_);
        return std::nullopt;
    }
    uint32_t address(size_t off) const { return base_ + uint32_t(off); }

    std::optional<Site> locate() const {
        if (!valid_)
            return std::nullopt;
        auto load = sandbox_kext_load_addr();
        if (!load)
            return std::nullopt;
        auto text = kext_text(*load);
        if (!text)
            return std::nullopt;
        auto text_off = offset(text->first);
        auto bad = find("bad opcode", 11), message = find("already in a sandbox, unable to switch to: ", 44);
        if (!text_off || !bad || !message || *text_off + text->second > n_)
            return std::nullopt;
        const uint32_t saddr = address(*bad), maddr = address(*message), taddr = text->first;
        std::vector<uint32_t> starts, refs, switch_refs;  // starts: push{..,lr} prologues and patched entries
        std::vector<std::pair<uint32_t, uint8_t>> flag_tests;
        std::map<int, uint32_t> reg_word;
        auto literal = [&](uint32_t la, int rt, uint32_t at) {
            auto lo = offset(la);
            if (!lo || *lo + 4 > n_)
                return;
            uint32_t w = u32(*lo);
            if (w == saddr)
                refs.push_back(at);
            else
                reg_word[rt] = w;
            if (w == maddr)
                switch_refs.push_back(at);
        };
        const size_t size = text->second;
        for (size_t pos = 0; pos + 1 < size;) {
            const size_t at = *text_off + pos;
            const uint32_t addr = taddr + uint32_t(pos);
            const uint16_t hw1 = u16(at);
            const size_t len = (hw1 & 0xF800) >= 0xE800 ? 4 : 2;
            if (len == 4 && pos + 3 >= size)
                break;
            const uint16_t hw2 = len == 4 ? u16(at + 2) : 0;
            if (pos + sizeof patch_bytes <= size && kind_at(at) != Site::stock)
                starts.push_back(addr);
            if (len == 2 && (hw1 & 0xFE00) == 0xB400 && (hw1 & 0x0100)) {
                starts.push_back(addr);  // push {.., lr}
            } else if (len == 4 && hw1 == 0xE92D && (hw2 & 0x4000)) {
                starts.push_back(addr);  // push.w {.., lr}
            } else if (len == 2 && (hw1 & 0xF800) == 0x4800) {
                // ldr rT, [pc, #imm]
                literal(((addr + 4) & ~3u) + (hw1 & 0xFF) * 4u, (hw1 >> 8) & 7, addr);
            } else if (len == 4 && (hw1 & 0xFF7F) == 0xF85F) {
                // ldr.w rT, [pc, #imm12]
                const uint32_t pc = (addr + 4) & ~3u, imm = hw2 & 0xFFF;
                literal((hw1 & 0x0080) ? pc + imm : pc - imm, (hw2 >> 12) & 0xF, addr);
            } else if (len == 2 && (hw1 & 0xFF78) == 0x4478) {
                // add rD, pc: a PC-relative delta loaded earlier
                const int rd = (hw1 & 7) | ((hw1 >> 4) & 8);
                if (auto w = reg_word.find(rd); w != reg_word.end()) {
                    const uint32_t resolved = addr + 4 + w->second;
                    if (resolved == saddr && rd < 8)
                        refs.push_back(addr);
                    if (resolved == maddr)
                        switch_refs.push_back(addr);
                }
            } else if (len == 4 && (hw1 & 0xFBF0) == 0xF010 && !(hw1 & 0x0400) && !(hw2 & 0xF000)) {
                // tst.w / ands.w rN, #imm8: a flag test
                const uint8_t imm = hw2 & 0xFF;
                if (imm && !(imm & (imm - 1)))
                    flag_tests.push_back({addr, imm});
            }
            pos += len;
        }
        if (refs.empty())
            return std::nullopt;
        const uint32_t x = *std::min_element(refs.begin(), refs.end());
        std::optional<uint32_t> entry;
        for (uint32_t s : starts)
            if (s <= x && (!entry || s > *entry))
                entry = s;
        if (!entry)
            return std::nullopt;
        // The hook's test sits within 12 bytes before or 8 after each reference to its message; all must agree.
        std::optional<uint8_t> bit;
        for (uint32_t r : switch_refs) {
            std::optional<std::pair<uint32_t, uint8_t>> best;
            for (const auto &t : flag_tests)
                if (t.first + 12 >= r && t.first <= r + 8 &&
                    (!best || distance(t.first, r) < distance(best->first, r)))
                    best = t;
            if (!best)
                continue;
            if (bit && *bit != best->second)
                return std::nullopt;
            bit = best->second;
        }
        if (!bit)
            return std::nullopt;
        Site site;
        site.offset = *offset(*entry);
        site.kind = kind_at(site.offset);
        site.switch_bit = *bit;
        return site;
    }

  private:
    struct Segment {
        std::string name;
        uint32_t vmaddr, vmsize, filesize;
    };
    struct Section {
        std::string sect, seg;
        uint32_t addr, size;
    };
    const uint8_t *b_;
    size_t n_;
    uint32_t base_ = 0;
    bool valid_ = false;
    std::vector<Segment> segments_;
    std::vector<Section> sections_;

    uint16_t u16(size_t o) const { return uint16_t(b_[o] | b_[o + 1] << 8); }
    uint32_t u32(size_t o) const { return uint32_t(b_[o]) | uint32_t(b_[o + 1]) << 8 | uint32_t(b_[o + 2]) << 16 | uint32_t(b_[o + 3]) << 24; }
    std::string name(size_t o) const {
        size_t end = o;
        while (end < o + 16 && b_[end])
            end++;
        return std::string(reinterpret_cast<const char *>(b_ + o), end - o);
    }
    static uint32_t distance(uint32_t a, uint32_t b) { return a > b ? a - b : b - a; }
    Site::Kind kind_at(size_t o) const {
        if (o + sizeof patch_bytes > n_)
            return Site::stock;
        if (std::memcmp(b_ + o, old_patch_bytes, sizeof old_patch_bytes) == 0)
            return Site::old_patch;
        for (size_t i = 0; i < sizeof patch_bytes; i++)
            if (i != flag_index && b_[o + i] != patch_bytes[i])
                return Site::stock;
        return Site::patch;
    }
    // The first occurrence of `n` bytes (a C string with its terminator) in the loaded kernel.
    std::optional<size_t> find(const char *text, size_t n) const {
        for (const auto &s : segments_) {
            auto lo = offset(s.vmaddr);
            if (!lo)
                continue;
            const size_t end = std::min(n_, *lo + std::min(s.vmsize, s.filesize));
            for (size_t i = *lo; i + n <= end; i++)
                if (b_[i] == uint8_t(text[0]) && std::memcmp(b_ + i, text, n) == 0)
                    return i;
        }
        return std::nullopt;
    }
    // The com.apple.security.sandbox kext's _PrelinkExecutableLoadAddr, from the occurrence of the key nearest its
    // bundle id (ahead of the id on 5.x-7.x, after it on 4.x).
    std::optional<uint32_t> sandbox_kext_load_addr() const {
        const Section *info = nullptr;
        for (const auto &s : sections_)
            if (s.sect == "__info" && s.seg == "__PRELINK_INFO")
                info = &s;
        if (!info)
            return std::nullopt;
        auto lo = offset(info->addr);
        if (!lo)
            return std::nullopt;
        const std::string text(reinterpret_cast<const char *>(b_ + *lo), std::min<size_t>(info->size, n_ - *lo));
        const size_t anchor = text.find("com.apple.security.sandbox");
        if (anchor == std::string::npos)
            return std::nullopt;
        const std::string key = "_PrelinkExecutableLoadAddr</key><integer";
        std::optional<std::pair<size_t, uint32_t>> before, after;
        for (size_t k = text.find(key); k != std::string::npos; k = text.find(key, k + key.size())) {
            const size_t gt = text.find('>', k + key.size());
            const size_t lt = gt == std::string::npos ? gt : text.find('<', gt + 1);
            if (lt == std::string::npos)
                continue;
            std::string value = text.substr(gt + 1, lt - gt - 1);
            value.erase(0, value.find_first_not_of(" \t\r\n"));
            if (value.rfind("0x", 0) != 0)
                continue;
            const uint32_t v = uint32_t(std::stoul(value.substr(2), nullptr, 16));
            if (k < anchor)
                before = {{k, v}};
            else if (!after)
                after = {{k, v}};
        }
        if (before && after)
            return anchor - before->first <= after->first - anchor ? before->second : after->second;
        if (before)
            return before->second;
        if (after)
            return after->second;
        return std::nullopt;
    }
    // The kext Mach-O's own __text (addr, size).
    std::optional<std::pair<uint32_t, uint32_t>> kext_text(uint32_t load) const {
        auto ko = offset(load);
        if (!ko || *ko + 28 > n_ || u32(*ko) != 0xFEEDFACE)
            return std::nullopt;
        size_t off = *ko + 28;
        for (uint32_t i = 0, cmds = u32(*ko + 16); i < cmds && off + 8 <= n_; i++) {
            const uint32_t cmd = u32(off), size = u32(off + 4);
            if (size < 8 || off + size > n_)
                break;
            if (cmd == 1) {
                size_t so = off + 56;
                for (uint32_t k = 0, nsects = u32(off + 48); k < nsects && so + 68 <= off + size; k++, so += 68)
                    if (name(so) == "__text")
                        return std::make_pair(u32(so + 32), u32(so + 36));
            }
            off += size;
        }
        return std::nullopt;
    }
};

// The kernel's Mach-O header in a kboot image: the first page-aligned 32-bit ARM header.
inline std::optional<size_t> kernel_header(const uint8_t *bytes, size_t size) {
    for (size_t o = 0; o + 28 <= size && o < (1u << 20); o += 0x1000)
        if (bytes[o] == 0xCE && bytes[o + 1] == 0xFA && bytes[o + 2] == 0xED && bytes[o + 3] == 0xFE && bytes[o + 4] == 12)
            return o;
    return std::nullopt;
}

struct Fix {
    size_t offset;
    uint8_t bytes[sizeof patch_bytes];
};
// Today's patch, with the kernel's own flag, where the image has v0.0.13's; nothing for a stock kernel, one already
// right, or one not understood.
inline std::optional<Fix> needed_fix(const uint8_t *bytes, size_t size) {
    auto header = kernel_header(bytes, size);
    if (!header)
        return std::nullopt;
    auto site = Image(bytes, size, *header).locate();
    if (!site || site->kind != Site::old_patch)
        return std::nullopt;
    Fix fix{site->offset, {}};
    std::memcpy(fix.bytes, patch_bytes, sizeof patch_bytes);
    fix.bytes[flag_index] = site->switch_bit;
    return fix;
}

}  // namespace lt::kernel_repair
