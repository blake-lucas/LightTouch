// A small C ABI over the shared web proxy (Portable/network), for the desktop worker. Every entry point catches the
// backend's exceptions and returns their message. On Unix the backend's own listener serves the engine's per-
// connection Unix socket forward; on Windows (AF_UNIX since Windows 10 1803) this file supplies the listener and
// hands each accepted socket to Proxy::serve, as Network.hpp intends for Windows frontends.
#include "Network.hpp"
#include <openssl/pem.h>
#include <openssl/x509.h>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <stdexcept>
#include <string>
#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <afunix.h>
#include <windows.h>
#include <wincrypt.h>
#include <atomic>
#include <condition_variable>
#include <cstdio>
#include <filesystem>
#include <mutex>
#include <set>
#include <thread>
#endif

namespace {
void copy(const std::string &text, char *out, size_t capacity) {
    if (!out || !capacity)
        return;
    size_t n = text.size() < capacity - 1 ? text.size() : capacity - 1;
    memcpy(out, text.data(), n);
    out[n] = 0;
}

lt::network::Configuration configuration(uint32_t mode, const char *date, uint32_t local) {
    lt::network::Configuration config;
    config.mode = mode == 1 ? lt::network::Mode::direct : mode == 2 ? lt::network::Mode::archive : lt::network::Mode::off;
    config.archive_date = date ? date : "";
    config.local_network = local != 0;
    return config;
}

#ifdef _WIN32
void winsock() {
    static std::once_flag once;
    std::call_once(once, [] {
        WSADATA data;
        WSAStartup(MAKEWORD(2, 2), &data);
    });
}

// The engine's guest forward connects once per guest TCP connection, like Proxy::listen's Unix acceptor.
struct Listener {
    SOCKET socket = INVALID_SOCKET;
    std::string path;
    std::thread acceptor;
    std::atomic<bool> stopping{false};
    std::mutex mutex;
    std::condition_variable idle;
    std::set<SOCKET> active;
};
#endif
} // namespace

struct lt_net_proxy {
    std::unique_ptr<lt::network::Proxy> proxy;
#ifdef _WIN32
    Listener listener;
#endif
};

struct lt_net_activity {
    uint64_t connections, active, completed, failed;
    uint32_t last_result_error;
    char summary[160];
    char last_result[512];
};

extern "C" {

int lt_net_valid_date(const char *date) {
    try {
        return date && lt::network::Configuration::valid_date(date);
    } catch (...) {
        return 0;
    }
}

// mode 1 direct, 2 archive. directory is the device's private proxy folder (its CA key lives there).
lt_net_proxy *lt_net_create(const char *directory, uint32_t mode, const char *date, uint32_t local, char *error,
                            size_t capacity) {
    try {
        if (!directory || !*directory || mode < 1 || mode > 2)
            throw std::runtime_error("Invalid network configuration");
#ifdef _WIN32
        winsock();
#endif
        auto out = std::make_unique<lt_net_proxy>();
        out->proxy = std::make_unique<lt::network::Proxy>(directory, configuration(mode, date, local));
        return out.release();
    } catch (const std::exception &e) {
        copy(e.what(), error, capacity);
    } catch (...) {
        copy("Network setup failed", error, capacity);
    }
    return nullptr;
}

// Applies to new requests; the engine's NAT/LAN policy is changed separately (lt_runtime_network_policy).
int lt_net_configure(lt_net_proxy *p, uint32_t mode, const char *date, uint32_t local, char *error, size_t capacity) {
    try {
        if (!p || mode > 2)
            throw std::runtime_error("Invalid network configuration");
        p->proxy->configure(configuration(mode, date, local));
        return 0;
    } catch (const std::exception &e) {
        copy(e.what(), error, capacity);
    } catch (...) {
        copy("Couldn't apply network settings", error, capacity);
    }
    return -1;
}

int lt_net_listen(lt_net_proxy *p, const char *path, char *error, size_t capacity) {
    try {
        if (!p || !path || !*path)
            throw std::runtime_error("Invalid proxy socket path");
#ifndef _WIN32
        p->proxy->listen(path);
#else
        auto &l = p->listener;
        if (strlen(path) >= sizeof(sockaddr_un::sun_path))
            throw std::runtime_error("Proxy socket path exceeds platform limit");
        SOCKET fd = socket(AF_UNIX, SOCK_STREAM, 0);
        if (fd == INVALID_SOCKET)
            throw std::runtime_error("Could not create proxy socket (" + std::to_string(WSAGetLastError()) + ")");
        sockaddr_un address{};
        address.sun_family = AF_UNIX;
        memcpy(address.sun_path, path, strlen(path) + 1);
        std::error_code ignored;
        std::filesystem::remove(std::filesystem::u8path(path), ignored);
        if (bind(fd, reinterpret_cast<sockaddr *>(&address), sizeof(address)) || ::listen(fd, 16)) {
            int code = WSAGetLastError();
            closesocket(fd);
            throw std::runtime_error("Could not bind private proxy socket (" + std::to_string(code) + ")");
        }
        l.socket = fd;
        l.path = path;
        auto proxy = p->proxy.get();
        l.acceptor = std::thread([&l, proxy, fd] {
            while (!l.stopping) {
                SOCKET peer = accept(fd, nullptr, nullptr);
                if (peer == INVALID_SOCKET)
                    break;
                DWORD ms = 30000;
                setsockopt(peer, SOL_SOCKET, SO_RCVTIMEO, reinterpret_cast<const char *>(&ms), sizeof(ms));
                setsockopt(peer, SOL_SOCKET, SO_SNDTIMEO, reinterpret_cast<const char *>(&ms), sizeof(ms));
                std::unique_lock<std::mutex> lock(l.mutex);
                if (l.stopping || l.active.size() >= 16) {
                    closesocket(peer);
                    continue;
                }
                l.active.insert(peer);
                lock.unlock();
                std::thread([&l, proxy, peer] {
                    try {
                        proxy->serve(std::intptr_t(peer));
                    } catch (...) {
                    }
                    std::lock_guard<std::mutex> guard(l.mutex);
                    closesocket(peer);
                    l.active.erase(peer);
                    l.idle.notify_all();
                }).detach();
            }
        });
#endif
        return 0;
    } catch (const std::exception &e) {
        copy(e.what(), error, capacity);
    } catch (...) {
        copy("Could not listen for the guest", error, capacity);
    }
    return -1;
}

// The proxy's public CA certificate (DER), for the guest's trust store. Returns its full size.
size_t lt_net_certificate(const lt_net_proxy *p, uint8_t *out, size_t capacity) {
    if (!p)
        return 0;
    auto der = p->proxy->certificate();
    if (out && capacity >= der.size())
        memcpy(out, der.data(), der.size());
    return der.size();
}

void lt_net_fingerprint(const lt_net_proxy *p, char *out, size_t capacity) {
    copy(p ? p->proxy->fingerprint() : std::string(), out, capacity);
}

void lt_net_activity_read(const lt_net_proxy *p, lt_net_activity *out) {
    if (!p || !out)
        return;
    auto a = p->proxy->activity();
    out->connections = a.connections;
    out->active = a.active;
    out->completed = a.completed;
    out->failed = a.failed;
    out->last_result_error = a.last_result_error;
    copy(a.summary(), out->summary, sizeof(out->summary));
    copy(a.last_result, out->last_result, sizeof(out->last_result));
}

void lt_net_destroy(lt_net_proxy *p) {
    if (!p)
        return;
#ifdef _WIN32
    auto &l = p->listener;
    l.stopping = true;
    if (l.socket != INVALID_SOCKET) {
        closesocket(l.socket);
        if (l.acceptor.joinable())
            l.acceptor.join();
        std::unique_lock<std::mutex> lock(l.mutex);
        for (auto socket : l.active)
            shutdown(socket, SD_BOTH);
        l.idle.wait(lock, [&] { return l.active.empty(); });
        lock.unlock();
        std::error_code ignored;
        std::filesystem::remove(std::filesystem::u8path(l.path), ignored);
    }
#endif
    delete p;
}

// Windows: write the system's trusted root certificates (the ROOT store) to path as PEM, for OpenSSL's
// SSL_CERT_FILE. Returns how many were written, or -1 with error.
int lt_net_export_system_roots(const char *path, char *error, size_t capacity) {
#ifdef _WIN32
    HCERTSTORE store = CertOpenSystemStoreW(0, L"ROOT");
    if (!store) {
        copy("Couldn't open the system's trusted certificates", error, capacity);
        return -1;
    }
    FILE *file = _wfopen(std::filesystem::u8path(path).wstring().c_str(), L"wb");
    if (!file) {
        CertCloseStore(store, 0);
        copy("Couldn't save the system's trusted certificates", error, capacity);
        return -1;
    }
    int count = 0;
    for (PCCERT_CONTEXT c = CertEnumCertificatesInStore(store, nullptr); c;
         c = CertEnumCertificatesInStore(store, c)) {
        const unsigned char *bytes = c->pbCertEncoded;
        X509 *x = d2i_X509(nullptr, &bytes, long(c->cbCertEncoded));
        if (x) {
            count += PEM_write_X509(file, x) == 1;
            X509_free(x);
        }
    }
    bool ok = fclose(file) == 0;
    CertCloseStore(store, 0);
    if (!ok) {
        copy("Couldn't save the system's trusted certificates", error, capacity);
        return -1;
    }
    return count;
#else
    (void)path;
    copy("Only Windows exports its certificate store", error, capacity);
    return -1;
#endif
}

} // extern "C"
