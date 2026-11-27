// The save-state zip (Zip.hpp): a stream entry and file entries round-trip, holes stay holes, and a file written
// against its base keeps only the chunks that differ from it (the iPod touch 2G's in-place NAND copy), read back by
// taking the rest from the base; without the base such an entry is refused.
#include "../src/Zip.hpp"
#include <cassert>
#include <cstring>
#include <fcntl.h>
#include <fstream>
#include <iostream>
#include <random>
#include <sys/stat.h>
#include <unistd.h>

static std::string read_all(const lt::fs::path &path) {
    std::ifstream in(path, std::ios::binary);
    assert(in.good());
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}
static void write_all(const lt::fs::path &path, const std::string &bytes) {
    std::ofstream out(path, std::ios::binary);
    out.write(bytes.data(), std::streamsize(bytes.size()));
    assert(out.good());
}

int main() {
    const auto dir = lt::fs::temp_directory_path() / ("lt-zip-test-" + std::to_string(getpid()));
    lt::fs::remove_all(dir);
    lt::fs::create_directories(dir);
    const size_t mb = lt::zip::chunk_bytes;

    // A base of 16 MiB of noise (incompressible) plus a partial last chunk; the live copy changes two chunks.
    std::mt19937 rng(7);
    std::string base(16 * mb + 1234, '\0');
    for (auto &c : base)
        c = char(rng());
    std::string live = base;
    live[3 * mb + 5] ^= 1;
    std::memset(&live[9 * mb], 0, mb); // a zero chunk in the live copy
    live[16 * mb + 7] ^= 2;            // the partial last chunk
    write_all(dir / "base", base);
    write_all(dir / "live", live);
    const std::string machine = "machine bytes " + std::string(3 * mb + 17, 'x');

    {
        int fd = open((dir / "s.zip").c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0600);
        assert(fd >= 0);
        lt::zip::Writer zip(fd, {});
        zip.begin("machine.state");
        zip.write(machine.data(), machine.size());
        zip.end();
        zip.file("state/nand/live", dir / "live", dir / "base");
        zip.file("plain", dir / "live");
        zip.finish();
        close(fd);
    }
    const auto size = lt::fs::file_size(dir / "s.zip");
    // Noise doesn't compress: the plain entry is ~16 MiB, the based one three chunks at most.
    assert(size > 16 * mb && size < 20 * mb);

    int fd = open((dir / "s.zip").c_str(), O_RDONLY);
    assert(fd >= 0);
    lt::zip::Reader reader(fd);
    std::string streamed;
    reader.read(*reader.find("machine.state"),
                [&](const uint8_t *p, size_t n) { streamed.append((const char *)p, n); });
    assert(streamed == machine);
    reader.extract(*reader.find("state/nand/live"), dir / "out-based", {}, dir / "base");
    assert(read_all(dir / "out-based") == live);
    reader.extract(*reader.find("plain"), dir / "out-plain");
    assert(read_all(dir / "out-plain") == live);
    struct stat st {};
    assert(stat((dir / "out-plain").c_str(), &st) == 0);
    assert(uint64_t(st.st_blocks) * 512 < live.size()); // the zero chunk is a hole
    bool refused = false;
    try {
        reader.extract(*reader.find("state/nand/live"), dir / "out-nobase");
    } catch (const std::exception &) {
        refused = true;
    }
    assert(refused);
    close(fd);

    // The plain entry alone measures what the based one saved.
    {
        int out = open((dir / "p.zip").c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0600);
        lt::zip::Writer zip(out, {});
        zip.file("state/nand/live", dir / "live", dir / "base");
        zip.finish();
        close(out);
    }
    assert(lt::fs::file_size(dir / "p.zip") < 4 * mb);
    lt::fs::remove_all(dir);
    std::cout << "zip ok (" << size << " bytes)\n";
    return 0;
}
