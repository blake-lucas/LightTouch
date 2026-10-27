// The few operating system calls the core makes, under one name on every system. On POSIX systems these pass
// straight through; on Windows they map onto the C runtime and Win32. Paths are UTF-8 everywhere: the Windows
// frontend runs with the UTF-8 ANSI code page (its manifest's activeCodePage), and the wide calls here convert
// explicitly anyway.
#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <string>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#ifdef _WIN32
#include <windows.h>
#include <winioctl.h>
#include <bcrypt.h>
#include <io.h>
#include <process.h>
#include <direct.h>
#include <cerrno>
#include <cstdlib>
#ifndef O_CLOEXEC
#define O_CLOEXEC 0
#endif
#ifndef O_NOFOLLOW
#define O_NOFOLLOW 0
#endif
#ifndef O_DIRECTORY
#define O_DIRECTORY 0x200000
#endif
#ifndef S_ISLNK
#define S_ISLNK(m) 0
#endif
#ifndef S_ISSOCK
#define S_ISSOCK(m) 0
#endif
#else
#include <dlfcn.h>
#include <sys/file.h>
#include <unistd.h>
#if defined(__linux__) || defined(__ANDROID__) || defined(__APPLE__)
#include <sys/random.h>
#endif
#endif

namespace lt::os {

#ifdef _WIN32
inline std::wstring wide(const char *utf8) {
    int n = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, nullptr, 0);
    std::wstring w(n > 0 ? size_t(n - 1) : 0, L'\0');
    if (n > 1)
        MultiByteToWideChar(CP_UTF8, 0, utf8, -1, w.data(), n);
    return w;
}

inline std::string narrow(const wchar_t *w) {
    int n = WideCharToMultiByte(CP_UTF8, 0, w, -1, nullptr, 0, nullptr, nullptr);
    std::string s(n > 0 ? size_t(n - 1) : 0, '\0');
    if (n > 1)
        WideCharToMultiByte(CP_UTF8, 0, w, -1, s.data(), n, nullptr, nullptr);
    return s;
}

inline void set_errno_from_win32() {
    switch (GetLastError()) {
    case ERROR_FILE_NOT_FOUND:
    case ERROR_PATH_NOT_FOUND: errno = ENOENT; break;
    case ERROR_ACCESS_DENIED:
    case ERROR_SHARING_VIOLATION:
    case ERROR_LOCK_VIOLATION: errno = EACCES; break;
    case ERROR_FILE_EXISTS:
    case ERROR_ALREADY_EXISTS: errno = EEXIST; break;
    case ERROR_DISK_FULL:
    case ERROR_HANDLE_DISK_FULL: errno = ENOSPC; break;
    default: errno = EIO;
    }
}

inline HANDLE handle(int fd) { return reinterpret_cast<HANDLE>(_get_osfhandle(fd)); }

inline int open(const wchar_t *path, int flags, int mode = 0) {
    if (flags & O_DIRECTORY) {
        // A directory handle, only ever used to flush it (a no-op here) or to inspect it.
        HANDLE h = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
                               OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
        if (h == INVALID_HANDLE_VALUE) {
            set_errno_from_win32();
            return -1;
        }
        int fd = _open_osfhandle(reinterpret_cast<intptr_t>(h), _O_RDONLY);
        if (fd < 0)
            CloseHandle(h);
        return fd;
    }
    // Files are shared for delete too, so a POSIX-style rename or unlink over an open file works.
    DWORD access = (flags & O_RDWR) ? GENERIC_READ | GENERIC_WRITE : (flags & O_WRONLY) ? GENERIC_WRITE : GENERIC_READ;
    DWORD disposition = OPEN_EXISTING;
    if ((flags & O_CREAT) && (flags & O_EXCL))
        disposition = CREATE_NEW;
    else if ((flags & O_CREAT) && (flags & O_TRUNC))
        disposition = CREATE_ALWAYS;
    else if (flags & O_CREAT)
        disposition = OPEN_ALWAYS;
    else if (flags & O_TRUNC)
        disposition = TRUNCATE_EXISTING;
    (void)mode;
    HANDLE h = CreateFileW(path, access, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, disposition,
                           FILE_ATTRIBUTE_NORMAL, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
        set_errno_from_win32();
        return -1;
    }
    // New files are sparse: NAND images are mostly holes, which the core never writes, and extending a file that
    // isn't sparse allocates its whole length (a prepared iPod touch 4G store is 17.7 GB, ~1.8 GB of data).
    if ((flags & O_CREAT) && (access & GENERIC_WRITE)) {
        DWORD ignored = 0;
        DeviceIoControl(h, FSCTL_SET_SPARSE, nullptr, 0, nullptr, 0, &ignored, nullptr);
    }
    int crt = _O_BINARY | ((flags & O_RDWR) ? _O_RDWR : (flags & O_WRONLY) ? _O_WRONLY : _O_RDONLY);
    if (flags & O_APPEND)
        crt |= _O_APPEND;
    int fd = _open_osfhandle(reinterpret_cast<intptr_t>(h), crt);
    if (fd < 0)
        CloseHandle(h);
    return fd;
}
inline int open(const char *path, int flags, int mode = 0) { return open(wide(path).c_str(), flags, mode); }

inline int close(int fd) { return ::_close(fd); }

inline ssize_t read(int fd, void *buffer, size_t size) {
    DWORD done = 0;
    if (!ReadFile(handle(fd), buffer, DWORD(size > 0x40000000 ? 0x40000000 : size), &done, nullptr)) {
        if (GetLastError() == ERROR_HANDLE_EOF)
            return 0;
        set_errno_from_win32();
        return -1;
    }
    return ssize_t(done);
}

inline ssize_t write(int fd, const void *buffer, size_t size) {
    DWORD done = 0;
    if (!WriteFile(handle(fd), buffer, DWORD(size > 0x40000000 ? 0x40000000 : size), &done, nullptr)) {
        set_errno_from_win32();
        return -1;
    }
    return ssize_t(done);
}

inline ssize_t pread(int fd, void *buffer, size_t size, int64_t offset) {
    OVERLAPPED at{};
    at.Offset = DWORD(uint64_t(offset));
    at.OffsetHigh = DWORD(uint64_t(offset) >> 32);
    DWORD done = 0;
    if (!ReadFile(handle(fd), buffer, DWORD(size > 0x40000000 ? 0x40000000 : size), &done, &at)) {
        if (GetLastError() == ERROR_HANDLE_EOF)
            return 0;
        set_errno_from_win32();
        return -1;
    }
    return ssize_t(done);
}

inline ssize_t pwrite(int fd, const void *buffer, size_t size, int64_t offset) {
    OVERLAPPED at{};
    at.Offset = DWORD(uint64_t(offset));
    at.OffsetHigh = DWORD(uint64_t(offset) >> 32);
    DWORD done = 0;
    if (!WriteFile(handle(fd), buffer, DWORD(size > 0x40000000 ? 0x40000000 : size), &done, &at)) {
        set_errno_from_win32();
        return -1;
    }
    return ssize_t(done);
}

inline int64_t lseek(int fd, int64_t offset, int whence) {
    LARGE_INTEGER to, now;
    to.QuadPart = offset;
    DWORD method = whence == SEEK_SET ? FILE_BEGIN : whence == SEEK_CUR ? FILE_CURRENT : FILE_END;
    if (!SetFilePointerEx(handle(fd), to, &now, method)) {
        set_errno_from_win32();
        return -1;
    }
    return now.QuadPart;
}

inline int fsync(int fd) {
    if (FlushFileBuffers(handle(fd)))
        return 0;
    // Directories (opened read-only for their handle) have nothing to flush here.
    BY_HANDLE_FILE_INFORMATION info{};
    if (GetFileInformationByHandle(handle(fd), &info) && (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY))
        return 0;
    // Flushing needs write access; POSIX code flushes through read-only descriptors too: flush the same file
    // through a writable handle. A read-only file (firmwarekit's outputs) refuses one: clear the attribute for it.
    if (GetLastError() == ERROR_ACCESS_DENIED) {
        wchar_t path[32768];
        DWORD n = GetFinalPathNameByHandleW(handle(fd), path, DWORD(sizeof path / sizeof *path), FILE_NAME_NORMALIZED);
        if (n > 0 && n < sizeof path / sizeof *path) {
            DWORD attributes = GetFileAttributesW(path);
            bool read_only = attributes != INVALID_FILE_ATTRIBUTES && (attributes & FILE_ATTRIBUTE_READONLY);
            if (read_only)
                SetFileAttributesW(path, attributes & ~DWORD(FILE_ATTRIBUTE_READONLY));
            HANDLE w = CreateFileW(path, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
                                   OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
            bool flushed = false;
            if (w != INVALID_HANDLE_VALUE) {
                flushed = FlushFileBuffers(w);
                CloseHandle(w);
            }
            if (read_only)
                SetFileAttributesW(path, attributes);
            if (flushed)
                return 0;
        }
    }
    set_errno_from_win32();
    return -1;
}

inline int ftruncate(int fd, int64_t size) {
    FILE_END_OF_FILE_INFO end{};
    end.EndOfFile.QuadPart = size;
    if (SetFileInformationByHandle(handle(fd), FileEndOfFileInfo, &end, sizeof end))
        return 0;
    set_errno_from_win32();
    return -1;
}

/// What the core reads of a file's metadata, with Windows' full 64-bit file index and 100 ns times.
struct Stat {
    uint32_t st_mode = 0;
    int64_t st_size = 0;
    uint64_t st_dev = 0, st_ino = 0;
    uint32_t st_uid = 0, st_nlink = 0;
    struct timespec st_mtim {}, st_ctim {};
};

inline void fill(Stat *st, const BY_HANDLE_FILE_INFORMATION &info) {
    *st = Stat{};
    // Owner-only modes, so the core's privacy checks hold: Windows profiles are per-user already.
    const bool directory = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY;
    const bool read_only = info.dwFileAttributes & FILE_ATTRIBUTE_READONLY;
    st->st_mode = directory ? (_S_IFDIR | (read_only ? 0500 : 0700)) : (_S_IFREG | (read_only ? 0400 : 0600));
    st->st_size = int64_t((uint64_t(info.nFileSizeHigh) << 32) | info.nFileSizeLow);
    st->st_nlink = info.nNumberOfLinks;
    st->st_dev = info.dwVolumeSerialNumber;
    st->st_ino = (uint64_t(info.nFileIndexHigh) << 32) | info.nFileIndexLow;
    auto unix_time = [](FILETIME t) {
        uint64_t v = (uint64_t(t.dwHighDateTime) << 32) | t.dwLowDateTime;
        uint64_t since = v - 116444736000000000ull;
        struct timespec ts {};
        ts.tv_sec = time_t(since / 10000000ull);
        ts.tv_nsec = long(since % 10000000ull) * 100;
        return ts;
    };
    st->st_mtim = unix_time(info.ftLastWriteTime);
    st->st_ctim = unix_time(info.ftCreationTime);
}

inline int fstat(int fd, Stat *st) {
    BY_HANDLE_FILE_INFORMATION info{};
    if (!GetFileInformationByHandle(handle(fd), &info)) {
        set_errno_from_win32();
        return -1;
    }
    fill(st, info);
    return 0;
}

inline int stat(const wchar_t *path, Stat *st) {
    HANDLE h = CreateFileW(path, FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
                           OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    if (h == INVALID_HANDLE_VALUE) {
        set_errno_from_win32();
        return -1;
    }
    BY_HANDLE_FILE_INFORMATION info{};
    bool ok = GetFileInformationByHandle(h, &info);
    CloseHandle(h);
    if (!ok) {
        set_errno_from_win32();
        return -1;
    }
    fill(st, info);
    return 0;
}
inline int stat(const char *path, Stat *st) { return stat(wide(path).c_str(), st); }
inline int lstat(const wchar_t *path, Stat *st) { return stat(path, st); }
inline int lstat(const char *path, Stat *st) { return stat(path, st); }
inline uint32_t euid() { return 0; }

// Windows has no owner-only modes: a mode without write bits is the read-only attribute (the core makes files
// writable again before it deletes them), anything else clears it.
inline int chmod(const wchar_t *path, int mode) {
    DWORD attributes = GetFileAttributesW(path);
    if (attributes == INVALID_FILE_ATTRIBUTES) {
        set_errno_from_win32();
        return -1;
    }
    DWORD wanted = (mode & 0222) ? (attributes & ~DWORD(FILE_ATTRIBUTE_READONLY)) : (attributes | FILE_ATTRIBUTE_READONLY);
    if (wanted == attributes || SetFileAttributesW(path, wanted))
        return 0;
    set_errno_from_win32();
    return -1;
}
inline int chmod(const char *path, int mode) { return chmod(wide(path).c_str(), mode); }
inline int fchmod(int, int) { return 0; }
/// Whether nothing may write `path` (a base's files): the read-only attribute. Windows ignores it on directories,
/// which hold no data of their own, so a directory counts as read-only.
inline bool read_only(const wchar_t *path) {
    DWORD attributes = GetFileAttributesW(path);
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_DIRECTORY));
}

/// An exclusive advisory lock, failing at once when another process holds it.
inline int lock_exclusive(int fd) {
    OVERLAPPED at{};
    if (LockFileEx(handle(fd), LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &at))
        return 0;
    errno = EWOULDBLOCK;
    return -1;
}

/// Rename, replacing an existing destination (POSIX rename).
inline int rename(const wchar_t *from, const wchar_t *to) {
    if (MoveFileExW(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
        return 0;
    set_errno_from_win32();
    return -1;
}
inline int rename(const char *from, const char *to) { return rename(wide(from).c_str(), wide(to).c_str()); }
inline int rename(const wchar_t *from, const char *to) { return rename(from, wide(to).c_str()); }
inline int rename(const char *from, const wchar_t *to) { return rename(wide(from).c_str(), to); }

/// Rename, failing if the destination exists.
inline int rename_noreplace(const wchar_t *from, const wchar_t *to) {
    if (MoveFileExW(from, to, MOVEFILE_WRITE_THROUGH))
        return 0;
    set_errno_from_win32();
    return -1;
}

inline int unlink(const wchar_t *path) {
    if (DeleteFileW(path))
        return 0;
    set_errno_from_win32();
    return -1;
}
inline int unlink(const char *path) { return unlink(wide(path).c_str()); }

inline bool exists(const char *path) { return GetFileAttributesW(wide(path).c_str()) != INVALID_FILE_ATTRIBUTES; }

inline int random_bytes(void *buffer, size_t size) {
    return BCryptGenRandom(nullptr, static_cast<PUCHAR>(buffer), ULONG(size), BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0
               ? 0
               : -1;
}

/// Replace the trailing XXXXXX of `templ` and create that directory (mode 0700 where modes exist).
inline char *mkdtemp(char *templ) {
    size_t n = std::strlen(templ);
    if (n < 6 || std::strcmp(templ + n - 6, "XXXXXX") != 0) {
        errno = EINVAL;
        return nullptr;
    }
    static const char alphabet[] = "abcdefghijklmnopqrstuvwxyz0123456789";
    for (int attempt = 0; attempt < 100; ++attempt) {
        unsigned char r[6];
        if (random_bytes(r, sizeof r) != 0)
            return nullptr;
        for (int i = 0; i < 6; ++i)
            templ[n - 6 + i] = alphabet[r[i] % 36];
        if (CreateDirectoryW(wide(templ).c_str(), nullptr))
            return templ;
        if (GetLastError() != ERROR_ALREADY_EXISTS) {
            set_errno_from_win32();
            return nullptr;
        }
    }
    errno = EEXIST;
    return nullptr;
}

inline int mkstemp(char *templ) {
    size_t n = std::strlen(templ);
    if (n < 6 || std::strcmp(templ + n - 6, "XXXXXX") != 0) {
        errno = EINVAL;
        return -1;
    }
    static const char alphabet[] = "abcdefghijklmnopqrstuvwxyz0123456789";
    for (int attempt = 0; attempt < 100; ++attempt) {
        unsigned char r[6];
        if (random_bytes(r, sizeof r) != 0)
            return -1;
        for (int i = 0; i < 6; ++i)
            templ[n - 6 + i] = alphabet[r[i] % 36];
        int fd = open(templ, O_RDWR | O_CREAT | O_EXCL, 0600);
        if (fd >= 0 || errno != EEXIST)
            return fd;
    }
    return -1;
}

inline int pid() { return _getpid(); }

using Library = HMODULE;

/// Load a shared library; its own dependencies are looked up beside it first.
inline Library library_open(const char *path, std::string &error) {
    HMODULE m = LoadLibraryExW(wide(path).c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    if (!m) {
        DWORD code = GetLastError();
        wchar_t *text = nullptr;
        FormatMessageW(FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                       nullptr, code, 0, reinterpret_cast<wchar_t *>(&text), 0, nullptr);
        error = text ? narrow(text) : "error " + std::to_string(code);
        if (text)
            LocalFree(text);
        while (!error.empty() && (error.back() == '\n' || error.back() == '\r' || error.back() == ' '))
            error.pop_back();
    }
    return m;
}
inline void *library_symbol(Library library, const char *name) {
    return reinterpret_cast<void *>(GetProcAddress(library, name));
}
inline void library_close(Library library) { FreeLibrary(library); }

#else
using ::close;
using ::fsync;
using ::read;
using ::write;
using ::unlink;
using ::mkdtemp;
using ::mkstemp;

inline int open(const char *path, int flags, int mode = 0) { return ::open(path, flags, mode); }
inline ssize_t pread(int fd, void *buffer, size_t size, int64_t offset) { return ::pread(fd, buffer, size, off_t(offset)); }
inline ssize_t pwrite(int fd, const void *buffer, size_t size, int64_t offset) {
    return ::pwrite(fd, buffer, size, off_t(offset));
}
inline int64_t lseek(int fd, int64_t offset, int whence) { return ::lseek(fd, off_t(offset), whence); }
inline int ftruncate(int fd, int64_t size) { return ::ftruncate(fd, off_t(size)); }
using Stat = struct ::stat;
inline int fstat(int fd, Stat *st) { return ::fstat(fd, st); }
inline int stat(const char *path, Stat *st) { return ::stat(path, st); }
inline int lstat(const char *path, Stat *st) { return ::lstat(path, st); }
inline uint32_t euid() { return uint32_t(::geteuid()); }
inline int chmod(const char *path, int mode) { return ::chmod(path, mode_t(mode)); }
inline int fchmod(int fd, int mode) { return ::fchmod(fd, mode_t(mode)); }
inline int lock_exclusive(int fd) { return ::flock(fd, LOCK_EX | LOCK_NB); }
/// Whether nothing may write `path`: no write bit for anyone.
inline bool read_only(const char *path) {
    struct ::stat st {};
    return ::stat(path, &st) == 0 && !(st.st_mode & 0222);
}
inline int rename(const char *from, const char *to) { return ::rename(from, to); }
inline bool exists(const char *path) { return ::access(path, F_OK) == 0; }
inline int pid() { return int(::getpid()); }

inline int random_bytes(void *buffer, size_t size) {
    auto *b = static_cast<unsigned char *>(buffer);
    size_t offset = 0;
    while (offset < size) {
#ifdef __APPLE__
        size_t chunk = size - offset > 256 ? 256 : size - offset;
        if (getentropy(b + offset, chunk) != 0)
            return -1;
        offset += chunk;
#else
        ssize_t n = getrandom(b + offset, size - offset, 0);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            return -1;
        offset += size_t(n);
#endif
    }
    return 0;
}

using Library = void *;

inline Library library_open(const char *path, std::string &error) {
    void *library = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!library) {
        const char *e = dlerror();
        error = e ? e : "unknown error";
    }
    return library;
}
inline void *library_symbol(Library library, const char *name) { return dlsym(library, name); }
inline void library_close(Library library) { dlclose(library); }
#endif

}  // namespace lt::os
