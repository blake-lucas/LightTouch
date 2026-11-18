// The kboot kernel repair's locator against real kernels: set LTM_SANDBOX_KERNELS to a tree of decrypted
// kernelcache.mach files (the Rust sandbox_patch corpus test reads the same tree). Each is laid out as kboot.bin lays
// it out; the stock kernel must resolve, v0.0.13's patch must be replaced by today's with the kernel's flag, and
// today's must be left alone.
#include "../src/KernelRepair.hpp"
#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iterator>

namespace fs = std::filesystem;
using namespace lt::kernel_repair;

static uint32_t le32(const std::vector<uint8_t> &b, size_t o) {
    return uint32_t(b[o]) | uint32_t(b[o + 1]) << 8 | uint32_t(b[o + 2]) << 16 | uint32_t(b[o + 3]) << 24;
}

// The kernel as kboot.bin holds it: each segment at its address less the base (lowest address & 0xF0000000).
static std::vector<uint8_t> loaded(const std::vector<uint8_t> &k, size_t &header) {
    struct Seg { uint32_t vmaddr, vmsize, fileoff, filesize; };
    std::vector<Seg> segs;
    size_t off = 28;
    for (uint32_t i = 0; i < le32(k, 16); i++) {
        uint32_t cmd = le32(k, off), size = le32(k, off + 4);
        if (cmd == 1 && le32(k, off + 24) != 0)
            segs.push_back({le32(k, off + 24), le32(k, off + 28), le32(k, off + 32), le32(k, off + 36)});
        off += size;
    }
    uint32_t lowest = 0xFFFFFFFF, end = 0;
    for (const auto &s : segs) {
        lowest = std::min(lowest, s.vmaddr);
        end = std::max(end, s.vmaddr + s.vmsize);
    }
    const uint32_t base = lowest & 0xF0000000u;
    std::vector<uint8_t> image(end - base);
    for (const auto &s : segs) {
        const uint32_t n = std::min(s.filesize, s.vmsize);
        std::copy(k.begin() + s.fileoff, k.begin() + s.fileoff + n, image.begin() + (s.vmaddr - base));
        if (s.fileoff == 0 && n)
            header = s.vmaddr - base;
    }
    return image;
}

int main() {
    const char *root = std::getenv("LTM_SANDBOX_KERNELS");
    if (!root) {
        std::puts("LTM_SANDBOX_KERNELS not set: nothing checked");
        return 0;
    }
    int checked = 0;
    for (const auto &e : fs::recursive_directory_iterator(root)) {
        if (e.path().filename() != "kernelcache.mach")
            continue;
        std::ifstream in(e.path(), std::ios::binary);
        std::vector<uint8_t> kernel((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        size_t header = 0;
        auto image = loaded(kernel, header);
        assert(kernel_header(image.data(), image.size()) == header);
        auto stock = Image(image.data(), image.size(), header).locate();
        if (!stock) {
            std::printf("%s: no sandbox kext found\n", e.path().string().c_str());
            continue;
        }
        assert(stock->kind == Site::stock && stock->switch_bit);
        assert(!needed_fix(image.data(), image.size()));
        // v0.0.13's patch: replaced by today's, with this kernel's flag.
        std::copy(std::begin(old_patch_bytes), std::end(old_patch_bytes), image.begin() + stock->offset);
        auto fix = needed_fix(image.data(), image.size());
        assert(fix && fix->offset == stock->offset && fix->bytes[flag_index] == stock->switch_bit);
        assert(std::equal(fix->bytes, fix->bytes + flag_index, patch_bytes));
        assert(std::equal(fix->bytes + flag_index + 1, std::end(fix->bytes), patch_bytes + flag_index + 1));
        // Today's patch: nothing to do.
        std::copy(std::begin(fix->bytes), std::end(fix->bytes), image.begin() + fix->offset);
        auto now = Image(image.data(), image.size(), header).locate();
        assert(now && now->kind == Site::patch && now->offset == stock->offset);
        assert(!needed_fix(image.data(), image.size()));
        std::printf("%s: sb_evaluate at image 0x%zx, switch bit 0x%02x\n", e.path().string().c_str(), stock->offset,
                    stock->switch_bit);
        checked++;
    }
    assert(checked > 0);
    return 0;
}
