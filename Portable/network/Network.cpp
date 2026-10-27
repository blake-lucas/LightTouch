// Shared LightTouch HTTP/TLS and Wayback backend. SPDX-License-Identifier: GPL-2.0-or-later
#include "Network.hpp"
#include <openssl/ssl.h>
#include <openssl/pem.h>
#include <openssl/x509v3.h>
#include <openssl/err.h>
#include <openssl/rand.h>
#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <functional>
#include <list>
#include <map>
#include <mutex>
#include <set>
#include <sstream>
#include <stdexcept>
#include <thread>
#include <vector>
#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <afunix.h>
#include <fcntl.h>
#include <io.h>
#include <sys/stat.h>
#else
#include <arpa/inet.h>
#include <netdb.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <fcntl.h>
#include <unistd.h>
#include <signal.h>
#include <pthread.h>
#endif
namespace lt::network {
namespace {
using namespace std::chrono_literals;
// SSL_write uses the platform socket directly. Suppress SIGPIPE only for this
// request thread, preserving the embedding application's signal policy.
struct PipeSignal {
#if !defined(_WIN32) && !defined(__APPLE__)
    sigset_t old{}, blocked{};
    bool restore = false;
    PipeSignal() {
        sigemptyset(&blocked); sigaddset(&blocked, SIGPIPE);
        restore = pthread_sigmask(SIG_BLOCK, &blocked, &old) == 0 && !sigismember(&old, SIGPIPE);
    }
    ~PipeSignal() {
        if (restore) {
            timespec zero{};
            while (sigtimedwait(&blocked, nullptr, &zero) >= 0) {}
            pthread_sigmask(SIG_SETMASK, &old, nullptr);
        }
    }
#endif
};
constexpr size_t header_limit = 65536, body_limit = 32 << 20, upload_limit = 8 << 20;
struct Error : std::runtime_error {
    int status;
    Error(int code, const std::string &message) : std::runtime_error(message), status(code) {}
};
void require(bool condition, int code, const std::string &message) { if (!condition) throw Error(code, message); }
std::string lower(std::string text) {
    for (char &c : text) if (c >= 'A' && c <= 'Z') c += 'a' - 'A';
    return text;
}
std::string trim(std::string text) {
    auto start = text.find_first_not_of(" \t\r\n"), end = text.find_last_not_of(" \t\r\n");
    return start == std::string::npos ? "" : text.substr(start, end - start + 1);
}
bool starts(const std::string &s, const std::string &prefix) { return s.rfind(prefix, 0) == 0; }
size_t number(const std::string &s, size_t maximum, int base = 10) {
    require(!s.empty() && s.find_first_not_of(base == 10 ? "0123456789" : "0123456789abcdefABCDEF") == std::string::npos,
            400, "Invalid message length");
    size_t result = 0;
    for (char c : s) {
        unsigned digit = c <= '9' ? c - '0' : (c | 32) - 'a' + 10;
        require(result <= (maximum - std::min<size_t>(digit, maximum)) / base && digit <= maximum, 413, "Message exceeds size limit");
        result = result * base + digit;
    }
    require(result <= maximum, 413, "Message exceeds size limit");
    return result;
}
// Paths arrive as UTF-8; Windows' narrow file calls would read them in the ANSI code page.
FILE *open_file(const std::string &path, const char *mode) {
#ifdef _WIN32
    std::wstring wide(mode, mode + strlen(mode));
    return _wfopen(std::filesystem::u8path(path).c_str(), wide.c_str());
#else
    return fopen(path.c_str(), mode);
#endif
}
#ifdef _WIN32
using Socket = SOCKET;
constexpr Socket invalid_socket = INVALID_SOCKET;
void close_socket(Socket fd) { closesocket(fd); }
#else
using Socket = int;
constexpr Socket invalid_socket = -1;
void close_socket(Socket fd) { close(fd); }
#endif
struct Connection {
    Socket fd = invalid_socket;
    SSL *tls = nullptr;
    bool owner = true;
    explicit Connection(Socket socket, bool own = true) : fd(socket), owner(own) {}
    ~Connection() { if (tls) { if (SSL_is_init_finished(tls)) SSL_shutdown(tls); SSL_free(tls); } if (owner && fd != invalid_socket) close_socket(fd); }
    int read(char *out, int length) { return tls ? SSL_read(tls, out, length) : recv(fd, out, length, 0); }
    void write(const std::string &bytes) {
        size_t offset = 0;
        while (offset < bytes.size()) {
#ifdef MSG_NOSIGNAL
            constexpr int flags = MSG_NOSIGNAL;
#else
            constexpr int flags = 0;
#endif
            int n = tls ? SSL_write(tls, bytes.data() + offset, int(bytes.size() - offset)) :
                          send(fd, bytes.data() + offset, int(bytes.size() - offset), flags);
            require(n > 0, 502, "Connection closed while writing"); offset += n;
        }
    }
    std::string line(size_t limit = header_limit) {
        std::string out;
        while (out.size() < limit) {
            char c;
            require(read(&c, 1) == 1, 400, "Connection closed before message completed");
            out += c;
            if (c == '\n') { require(out.size() >= 2 && out[out.size()-2] == '\r', 400, "Invalid HTTP line ending"); return out.substr(0, out.size() - 2); }
        }
        throw Error(431, "HTTP headers exceed size limit");
    }
    std::string exact(size_t length) {
        std::string out(length, '\0'); size_t offset = 0;
        while (offset < length) {
            int n = read(out.data() + offset, int(length - offset));
            require(n > 0, 502, "Incomplete response body"); offset += n;
        }
        return out;
    }
};
void timeout(Socket socket, unsigned seconds) {
#ifdef _WIN32
    DWORD ms = seconds * 1000;
    setsockopt(socket, SOL_SOCKET, SO_RCVTIMEO, reinterpret_cast<const char *>(&ms), sizeof(ms));
    setsockopt(socket, SOL_SOCKET, SO_SNDTIMEO, reinterpret_cast<const char *>(&ms), sizeof(ms));
#else
    timeval limit{long(seconds), 0};
    setsockopt(socket, SOL_SOCKET, SO_RCVTIMEO, &limit, sizeof(limit));
    setsockopt(socket, SOL_SOCKET, SO_SNDTIMEO, &limit, sizeof(limit));
#ifdef SO_NOSIGPIPE
    int on = 1; setsockopt(socket, SOL_SOCKET, SO_NOSIGPIPE, &on, sizeof(on));
#endif
#endif
}
using Headers = std::map<std::string, std::string>;
Headers headers(Connection &c, std::vector<std::string> *cookies = nullptr) {
    Headers out; size_t total = 0;
    for (;;) {
        std::string line = c.line(); total += line.size() + 2;
        require(total < header_limit, 431, "HTTP headers exceed size limit");
        if (line.empty()) break;
        size_t colon = line.find(':');
        require(colon != std::string::npos && colon > 0, 400, "Invalid HTTP header");
        std::string name = lower(line.substr(0, colon)), value = trim(line.substr(colon + 1));
        require(name.find_first_not_of("abcdefghijklmnopqrstuvwxyz0123456789!#$%&'*+-.^_`|~") == std::string::npos &&
                value.find_first_of("\r\n\0", 0, 3) == std::string::npos, 400, "Invalid HTTP header");
        require(!out.count(name) || (name != "content-length" && name != "transfer-encoding" && name != "host"), 400, "Ambiguous HTTP framing");
        if (name == "set-cookie" && cookies) cookies->push_back(value);
        else if (out.count(name) && name != "set-cookie") out[name] += ", " + value;
        else out[name] = value;
    }
    require(!(out.count("content-length") && out.count("transfer-encoding")), 400, "Ambiguous HTTP framing");
    return out;
}
std::string body(Connection &c, const Headers &h, bool until_close, size_t limit) {
    auto transfer = h.find("transfer-encoding"), length = h.find("content-length");
    if (transfer != h.end()) {
        require(lower(transfer->second) == "chunked", 501, "Unsupported transfer encoding");
        std::string out;
        for (;;) {
            auto line = c.line(1024); auto semi = line.find(';');
            size_t size = number(line.substr(0, semi), limit - out.size(), 16);
            if (!size) { (void)headers(c); return out; }
            out += c.exact(size);
            require(c.exact(2) == "\r\n", 400, "Invalid chunk framing");
        }
    }
    if (length != h.end()) return c.exact(number(length->second, limit));
    std::string out;
    if (until_close) {
        std::array<char, 16384> chunk{};
        for (;;) { int n = c.read(chunk.data(), chunk.size()); if (!n) break; require(n > 0, 502, "Upstream connection failed");
            require(out.size() + n <= limit, 413, "Response exceeds size limit"); out.append(chunk.data(), n); }
    }
    return out;
}
std::string authority(const Address &a) {
    auto host = a.host.find(':') != std::string::npos ? "[" + a.host + "]" : a.host;
    return host + ((a.scheme == "https" && a.port == 443) || (a.scheme == "http" && a.port == 80) ? "" : ":" + std::to_string(a.port));
}
void canonical(const unsigned char *&b, size_t &size) {
    if (size != 16) return;
    bool zero10 = std::all_of(b,b+10,[](unsigned char x){return !x;});
    bool zero12 = std::all_of(b,b+12,[](unsigned char x){return !x;});
    const unsigned char nat64[]{0,0x64,0xff,0x9b,0,0,0,0,0,0,0,0};
    bool translated = std::all_of(b,b+8,[](unsigned char x){return !x;}) && b[8]==255 && b[9]==255 && !b[10] && !b[11];
    if ((zero10 && b[10]==255 && b[11]==255) || zero12 || translated || !memcmp(b,nat64,12)) { b+=12;size=4; }
}
bool allowed(const sockaddr *sa, bool local) {
    const unsigned char *bytes; size_t length;
    if (sa->sa_family == AF_INET) { bytes = reinterpret_cast<const unsigned char *>(&reinterpret_cast<const sockaddr_in *>(sa)->sin_addr); length = 4; }
    else if (sa->sa_family == AF_INET6) { bytes = reinterpret_cast<const unsigned char *>(&reinterpret_cast<const sockaddr_in6 *>(sa)->sin6_addr); length = 16; }
    else return false;
    canonical(bytes,length);
    if (length == 4 && (!bytes[0] || bytes[0] == 127)) return false;
    return local || public_address(bytes, length);
}
Socket connect_to(const Address &a, bool local) {
    auto host = lower(a.host);
    require(host != "localhost" && !(host.size() > 10 && host.substr(host.size()-10) == ".localhost"), 403, "Host loopback access is unavailable");
    addrinfo hints{}; hints.ai_socktype = SOCK_STREAM; hints.ai_family = AF_UNSPEC;
    addrinfo *raw = nullptr;
    require(getaddrinfo(a.host.c_str(), std::to_string(a.port).c_str(), &hints, &raw) == 0, 502, "DNS lookup failed");
    std::unique_ptr<addrinfo, decltype(&freeaddrinfo)> addresses(raw, freeaddrinfo);
    for (auto p = raw; p; p = p->ai_next) require(allowed(p->ai_addr, local), 403, "Local network access is off");
    for (auto p = raw; p; p = p->ai_next) {
        Socket fd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
        if (fd == invalid_socket) continue;
        timeout(fd, 15);
#ifndef _WIN32
        // A bounded connect; SO_SNDTIMEO alone is not a portable connect timeout.
        int flags = fcntl(fd, F_GETFL); fcntl(fd, F_SETFL, flags | O_NONBLOCK);
        int rc = connect(fd, p->ai_addr, p->ai_addrlen);
        if (rc && errno == EINPROGRESS) {
            pollfd poller{fd, POLLOUT, 0}; rc = poll(&poller, 1, 10000);
            int error = 0; socklen_t size = sizeof(error);
            rc = rc > 0 && !getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &size) && !error ? 0 : -1;
        }
        fcntl(fd, F_SETFL, flags);
#else
        int rc = connect(fd, p->ai_addr, int(p->ai_addrlen));
#endif
        if (!rc) return fd;
        close_socket(fd);
    }
    throw Error(502, "Could not connect to the website");
}
// `capture`: the archive's yyyyMMdd day of the copy it served (empty when it had none, or for direct replies).
struct Reply { int status = 502; Headers headers; std::string body; std::vector<std::string> cookies{}; std::string capture{}; };
bool hop(const std::string &name) {
    static const std::set<std::string> names{"connection", "proxy-connection", "keep-alive", "transfer-encoding", "te", "trailer", "upgrade", "proxy-authorization", "proxy-authenticate", "content-length"};
    return names.count(name);
}
std::string response(const Reply &r, bool head = false) {
    std::string out = "HTTP/1.1 " + std::to_string(r.status) + " Response\r\nConnection: close\r\n";
    for (const auto &h : r.headers) if (!hop(h.first) && h.first != "strict-transport-security" && h.first != "alt-svc") out += h.first + ": " + h.second + "\r\n";
    for (const auto &cookie : r.cookies) out += "Set-Cookie: " + cookie + "\r\n";
    if (!head) out += "Content-Length: " + std::to_string(r.body.size()) + "\r\n";
    return out + "\r\n" + (head ? "" : r.body);
}
using Key = std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)>;
using Cert = std::unique_ptr<X509, decltype(&X509_free)>;
Key new_key() {
    EVP_PKEY *p = nullptr;
    EVP_PKEY_CTX *c = EVP_PKEY_CTX_new_id(EVP_PKEY_RSA, nullptr);
    require(c, 503, "Could not prepare TLS key");
    bool ok = EVP_PKEY_keygen_init(c) > 0 && EVP_PKEY_CTX_set_rsa_keygen_bits(c, 2048) > 0 && EVP_PKEY_keygen(c, &p) > 0;
    EVP_PKEY_CTX_free(c); require(ok, 503, "Could not prepare TLS key"); return Key(p, EVP_PKEY_free);
}
void extension(X509 *cert, X509 *issuer, int nid, const std::string &value) {
    X509V3_CTX context{}; X509V3_set_ctx(&context, issuer, cert, nullptr, nullptr, 0);
    X509_EXTENSION *ex = X509V3_EXT_conf_nid(nullptr, &context, nid, value.c_str());
    require(ex, 503, "Could not encode certificate");
    int rc = X509_add_ext(cert, ex, -1); X509_EXTENSION_free(ex); require(rc == 1, 503, "Could not encode certificate");
}
Cert make_cert(EVP_PKEY *key, EVP_PKEY *issuer_key, X509 *issuer, const std::string &name) {
    Cert cert(X509_new(), X509_free); require(bool(cert), 503, "Certificate allocation failed");
    X509_set_version(cert.get(), 2);
    unsigned char serial[16]; require(RAND_bytes(serial, sizeof(serial)) == 1, 503, "Random source failed"); serial[0] &= 0x7f;
    BIGNUM *bn = BN_bin2bn(serial, sizeof(serial), nullptr); ASN1_INTEGER *integer = BN_to_ASN1_INTEGER(bn, nullptr);
    X509_set_serialNumber(cert.get(), integer); ASN1_INTEGER_free(integer); BN_free(bn);
    X509_set_pubkey(cert.get(), key);
    // A guest may keep its historical RTC; validity starts before all supported iOS releases.
    ASN1_TIME_set_string(X509_getm_notBefore(cert.get()), "20000101000000Z");
    X509_gmtime_adj(X509_getm_notAfter(cert.get()), issuer ? 30L*86400 : 3650L*86400);
    // Built apart and copied in: OpenSSL 4 hands out a certificate's own names as const.
    std::unique_ptr<X509_NAME, decltype(&X509_NAME_free)> subject(X509_NAME_new(), X509_NAME_free);
    require(subject && X509_NAME_add_entry_by_txt(subject.get(), "CN", MBSTRING_ASC,
                                                  reinterpret_cast<const unsigned char *>(name.c_str()), -1, -1, 0) == 1,
            503, "Could not encode certificate");
    X509_set_subject_name(cert.get(), subject.get());
    X509_set_issuer_name(cert.get(), issuer ? X509_get_subject_name(issuer) : subject.get());
    extension(cert.get(), issuer ? issuer : cert.get(), NID_basic_constraints, issuer ? "critical,CA:FALSE" : "critical,CA:TRUE,pathlen:0");
    extension(cert.get(), issuer ? issuer : cert.get(), NID_key_usage, issuer ? "critical,digitalSignature,keyEncipherment" : "critical,keyCertSign,cRLSign");
    if (issuer) {
        unsigned char ip[16]; bool numeric = inet_pton(AF_INET, name.c_str(), ip) == 1 || inet_pton(AF_INET6, name.c_str(), ip) == 1;
        extension(cert.get(), issuer, NID_subject_alt_name, (numeric ? "IP:" : "DNS:") + name);
        extension(cert.get(), issuer, NID_ext_key_usage, "serverAuth");
    }
    // SHA-1 is confined to the local legacy guest leg, as in upstream. Host TLS keeps modern verification.
    require(X509_sign(cert.get(), issuer_key, EVP_sha1()) > 0, 503, "Could not sign guest certificate");
    return cert;
}
} // namespace
bool Configuration::valid_date(const std::string &date) {
    if (date.size() != 8 || date.find_first_not_of("0123456789") != std::string::npos) return false;
    int year = std::stoi(date.substr(0,4)), month = std::stoi(date.substr(4,2)), day = std::stoi(date.substr(6));
    const int days[]{31,28,31,30,31,30,31,31,30,31,30,31};
    if (year < 1996 || year > 9999 || month < 1 || month > 12) return false;
    return day >= 1 && day <= days[month-1] + (month == 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0));
}
Address Address::parse(const std::string &url) {
    require(url.size() <= 8192 && url.find_first_of("\r\n\t \\\0", 0, 6) == std::string::npos, 400, "Invalid web address");
    size_t scheme_end = url.find("://"); require(scheme_end != std::string::npos, 400, "Expected an HTTP address");
    Address a; a.scheme = lower(url.substr(0, scheme_end));
    require(a.scheme == "http" || a.scheme == "https", 400, "Only HTTP and HTTPS are supported");
    size_t end = url.find_first_of("/?#", scheme_end + 3);
    std::string host = url.substr(scheme_end + 3, end == std::string::npos ? end : end - scheme_end - 3);
    require(!host.empty() && host.find('@') == std::string::npos && host.find('%') == std::string::npos, 400, "Invalid web host");
    a.port = a.scheme == "https" ? 443 : 80;
    if (host[0] == '[') {
        size_t bracket = host.find(']'); require(bracket != std::string::npos, 400, "Invalid IPv6 address");
        a.host = host.substr(1, bracket-1);
        if (bracket+1 < host.size()) { require(host[bracket+1] == ':', 400, "Invalid web port"); a.port = number(host.substr(bracket+2), 65535); }
    } else {
        size_t colon = host.find(':'); a.host = lower(host.substr(0, colon));
        if (colon != std::string::npos) a.port = number(host.substr(colon+1), 65535);
    }
    require(a.port && !a.host.empty() && a.host.size() <= 253 && a.host.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-:") == std::string::npos, 400, "Invalid web host");
    a.path = end == std::string::npos ? "/" : url.substr(end);
    if (a.path[0] != '/') a.path = "/" + a.path;
    auto fragment = a.path.find('#'); if (fragment != std::string::npos) a.path.resize(fragment);
    return a;
}
std::string Address::url() const { return scheme + "://" + authority(*this) + path; }
bool public_address(const unsigned char *b, size_t size) {
    canonical(b,size);
    if (size == 16) {
        return (b[0]&0xfe)!=0xfc && !(b[0]==0xfe && (b[1]&0x80)) && b[0]!=0xff;
    }
    if (size != 4) return false;
    return b[0] && b[0]!=127 && b[0]!=10 && !(b[0]==172 && (b[1]&0xf0)==16) && !(b[0]==192 && b[1]==168) &&
        !(b[0]==169 && b[1]==254) && !(b[0]==100 && (b[1]&0xc0)==64) && !(b[0]==198 && (b[1]&0xfe)==18) && b[0]<224;
}
std::string archive_url(const std::string &target, const std::string &date) {
    require(Configuration::valid_date(date), 400, "Choose a valid archive date");
    return "https://web.archive.org/web/" + date + "id_/" + Address::parse(target).url();
}
// The day of the capture an archive URL names (/web/yyyyMMddhhmmssid_/...), once its redirect has picked one.
std::string capture_day(const std::string &url) {
    const std::string prefix = "https://web.archive.org/web/";
    if (!starts(url, prefix) || url.size() < prefix.size() + 18 || url.compare(prefix.size() + 14, 4, "id_/") != 0) return "";
    for (size_t i = prefix.size(); i < prefix.size() + 14; i++) if (url[i] < '0' || url[i] > '9') return "";
    return url.substr(prefix.size(), 8);
}
// "9 September 2009" from yyyyMMdd.
std::string long_day(const std::string &day) {
    static const char *months[] = {"January","February","March","April","May","June","July","August","September","October","November","December"};
    if (day.size() != 8 || day.find_first_not_of("0123456789") != std::string::npos) return day;
    int month = std::stoi(day.substr(4, 2));
    if (month < 1 || month > 12) return day;
    return std::to_string(std::stoi(day.substr(6, 2))) + " " + months[month - 1] + " " + day.substr(0, 4);
}
std::string archive_redirect(const std::string &current, const std::string &location, const std::string &date) {
    require(!location.empty(), 502, "Archive returned an empty redirect");
    if (starts(location, "https://web.archive.org/web/") || starts(location, "http://web.archive.org/web/")) {
        auto path = Address::parse(location).path; return "https://web.archive.org" + path;
    }
    if (starts(location, "/web/")) return "https://web.archive.org" + location;
    if (starts(location, "http://") || starts(location,"https://")) return archive_url(location,date);
    if (starts(location,"//")) return archive_url("https:"+location,date);
    // Relative origin redirects stay within the dated original site, never the live web.
    auto mark = current.find("id_/"); require(mark != std::string::npos,502,"Invalid archive redirect");
    Address original = Address::parse(current.substr(mark+4));
    original.path = location[0]=='/' ? location : original.path.substr(0,original.path.rfind('/')+1)+location;
    return archive_url(original.url(),date);
}
struct Proxy::State {
    Configuration config;
    mutable std::mutex config_mutex;
    Configuration configuration() const { std::lock_guard<std::mutex> lock(config_mutex); return config; }
    Key key{nullptr,EVP_PKEY_free}, leaf_key{nullptr,EVP_PKEY_free};
    Cert ca{nullptr,X509_free};
    std::unique_ptr<SSL_CTX, decltype(&SSL_CTX_free)> client{nullptr,SSL_CTX_free};
    std::string der, socket_path;
    std::atomic<bool> stopping{false};
    std::atomic<Socket> listener{invalid_socket};
    std::thread acceptor;
    mutable std::mutex mutex;
    std::condition_variable idle;
    std::set<Socket> active;
    Activity activity;
    // The archive's cache and back-off, and how many of its requests are out.
    std::mutex gate;
    std::condition_variable archive_slot;
    int archive_requests = 0;
    std::chrono::steady_clock::time_point cooldown{};
    struct Cached { std::string target, date; Reply reply; };
    std::list<Cached> cache;
    size_t cache_bytes = 0;
    void begin() { std::lock_guard<std::mutex> lock(mutex); ++activity.connections; ++activity.active; }
    void finish(const std::string &result, bool error) {
        std::lock_guard<std::mutex> lock(mutex);
        --activity.active; ++activity.completed;
        if (error) ++activity.failed;
        activity.last_result = result; activity.last_result_error = error;
    }
    std::unique_ptr<Connection> open(const Configuration &config, const Address &address) {
        auto origin = std::make_unique<Connection>(connect_to(address, config.local_network && config.mode == Mode::direct));
        if (address.scheme == "https") {
            origin->tls = SSL_new(client.get()); require(origin->tls,503,"TLS allocation failed");
            SSL_set_fd(origin->tls,int(origin->fd)); SSL_set_tlsext_host_name(origin->tls,address.host.c_str());
            auto param = SSL_get0_param(origin->tls);
            unsigned char ip[16]; bool numeric = inet_pton(AF_INET,address.host.c_str(),ip)==1 || inet_pton(AF_INET6,address.host.c_str(),ip)==1;
            require((numeric ? X509_VERIFY_PARAM_set1_ip_asc(param,address.host.c_str()) : SSL_set1_host(origin->tls,address.host.c_str())) == 1,502,"Invalid TLS host");
            require(SSL_connect(origin->tls)==1 && SSL_get_verify_result(origin->tls)==X509_V_OK,502,"Website TLS verification failed");
        }
        return origin;
    }
    // One request and its response on `origin`; `answered` once a status line arrived, `reusable` when the
    // connection can carry another (keep-alive asked and granted, the body framed).
    Reply exchange(Connection &origin, const Address &address, const std::string &method, const Headers &request_headers, const std::string &upload, bool keep, bool &answered, bool &reusable) {
        std::string wire = method + " " + address.path + " HTTP/1.1\r\nHost: " + authority(address) + (keep ? "\r\nConnection: keep-alive" : "\r\nConnection: close") + "\r\nAccept-Encoding: identity\r\n";
        std::set<std::string> connection_names;
        auto named = request_headers.find("connection");
        if (named != request_headers.end()) { std::istringstream words(lower(named->second)); std::string token; while (std::getline(words,token,',')) connection_names.insert(trim(token)); }
        for (auto &h : request_headers) if (!hop(h.first) && h.first!="host" && h.first!="accept-encoding" && !connection_names.count(h.first)) wire += h.first + ": " + h.second + "\r\n";
        if (!upload.empty() || request_headers.count("content-length")) wire += "Content-Length: " + std::to_string(upload.size()) + "\r\n";
        origin.write(wire + "\r\n" + upload);
        Reply reply; std::string version;
        for (int i=0;i<5;i++) {
            auto line=origin.line(); answered=true; std::istringstream stream(line);
            stream>>version>>reply.status; require(starts(version,"HTTP/1.") && reply.status>=100 && reply.status<=599,502,"Invalid upstream response");
            reply.cookies.clear(); reply.headers = headers(origin,&reply.cookies); if (reply.status>=200) break;
        }
        require(reply.status>=200,502,"Too many interim responses");
        const bool bodyless = method=="HEAD" || reply.status==204 || reply.status==304;
        if (!bodyless) reply.body=body(origin,reply.headers,true,body_limit);
        reusable = keep && version=="HTTP/1.1" && (bodyless || reply.headers.count("content-length") || reply.headers.count("transfer-encoding")) &&
                   !(reply.headers.count("connection") && lower(reply.headers["connection"]).find("close")!=std::string::npos);
        // Remove headers explicitly named by Connection, too.
        if (reply.headers.count("connection")) { std::istringstream words(lower(reply.headers["connection"])); std::string token; while (std::getline(words,token,',')) { token=trim(token); if (token=="set-cookie") reply.cookies.clear(); if (token!="connection") reply.headers.erase(token); } }
        return reply;
    }
    // Idle connections to the Internet Archive, kept like a browser keeps its own: a new TCP and TLS handshake for
    // every request and redirect was most of each resource's time.
    std::mutex pool_mutex;
    std::vector<std::unique_ptr<Connection>> archive_pool;
    Reply fetch(const Configuration &config, const std::string &url, const std::string &method, const Headers &request_headers = {}, const std::string &upload = "", bool keep = false) {
        Address address = Address::parse(url);
        keep = keep && address.scheme == "https" && lower(address.host) == "web.archive.org" && address.port == 443;
        for (int attempt = 0;; attempt++) {
            std::unique_ptr<Connection> origin;
            if (keep && attempt == 0) {
                std::lock_guard<std::mutex> lock(pool_mutex);
                if (!archive_pool.empty()) { origin = std::move(archive_pool.back()); archive_pool.pop_back(); }
            }
            const bool pooled = bool(origin);
            if (!origin) origin = open(config, address);
            bool answered = false, reusable = false;
            try {
                Reply reply = exchange(*origin, address, method, request_headers, upload, keep, answered, reusable);
                if (reusable) {
                    std::lock_guard<std::mutex> lock(pool_mutex);
                    if (archive_pool.size() < 32) archive_pool.push_back(std::move(origin));
                }
                return reply;
            } catch (const std::exception &) {
                // A kept connection the server has since closed fails before any answer: once more on a new one.
                if (pooled && !answered) continue;
                throw;
            }
        }
    }
    Reply archived(const Configuration &config, const std::string &target, const std::string &method) {
        std::unique_lock<std::mutex> lock(gate);
        for (auto it=cache.begin();it!=cache.end();++it) if (it->target==target && it->date==config.archive_date) { Reply r=it->reply; cache.splice(cache.begin(),cache,it); return r; }
        // Six at once, each on its own kept-alive connection: what a browser speaking HTTP/1.1 opens to one site.
        // Firefox gets further with HTTP/2 (up to 58 requests in flight for a 2009 nytimes.com) but over just two
        // connections; dozens of separate connections from one address get it refused by the Archive for a while.
        // The guest's Safari asks for about six at a time itself. A 429/503 stops every request for its Retry-After.
        archive_slot.wait(lock,[&]{return archive_requests<6;});
        ++archive_requests;
        lock.unlock();
        struct Slot { State &s; ~Slot() { { std::lock_guard<std::mutex> l(s.gate); --s.archive_requests; } s.archive_slot.notify_one(); } } slot{*this};
        auto url = archive_url(target,config.archive_date);
        for (int redirects=0;redirects<8;redirects++) {
            auto now=std::chrono::steady_clock::now();
            {
                std::lock_guard<std::mutex> guard(gate);
                if (now<cooldown) return {429,{{"content-type","text/plain"},{"retry-after",std::to_string(std::chrono::duration_cast<std::chrono::seconds>(cooldown-now).count()+1)}},"The Internet Archive asked LightTouch to wait. Try reloading after the wait shown in Retry-After.\n"};
            }
            Reply r;
            try {
                r=fetch(config,url,method,{{"user-agent","LightTouch/1.0 (archive replay)"}},"",true);
            } catch (const Error &error) {
                // A refused connection is the Archive limiting this address for a while: pause rather than retry
                // every resource, which would only keep it refusing.
                if (error.status!=502 || std::string(error.what())!="Could not connect to the website") throw;
                { std::lock_guard<std::mutex> guard(gate); cooldown=std::max(cooldown,std::chrono::steady_clock::now()+60s); }
                throw Error(503,"The Internet Archive isn't accepting connections from this network right now (it limits busy addresses for a while). Try again later.");
            }
            if (r.status==429 || r.status==503) {
                int seconds=60;
                try { if (r.headers.count("retry-after")) seconds=int(number(r.headers["retry-after"],3600)); } catch (...) {}
                std::lock_guard<std::mutex> guard(gate);
                cooldown=std::max(cooldown,std::chrono::steady_clock::now()+std::chrono::seconds(std::max(1,seconds)));
                r.headers["retry-after"]=std::to_string(seconds); return r;
            }
            if (r.status>=300 && r.status<400 && r.headers.count("location")) { url=archive_redirect(url,r.headers["location"],config.archive_date); continue; }
            r.headers.erase("set-cookie"); r.cookies.clear();
            r.capture = capture_day(url);
            // A few pages' worth (a 2009 front page is ~60 resources): going back to a page doesn't refetch it all.
            if (method=="GET" && r.status==200 && r.body.size()<=2<<20) {
                std::lock_guard<std::mutex> guard(gate);
                while (!cache.empty() && (cache.size()>=256 || cache_bytes+r.body.size()>32<<20)) { cache_bytes-=cache.back().reply.body.size(); cache.pop_back(); }
                cache_bytes+=r.body.size(); cache.push_front({target,config.archive_date,r});
            }
            return r;
        }
        throw Error(502,"The Internet Archive returned too many redirects");
    }
};
Proxy::Proxy(std::string directory, Configuration config, std::string roots) : state(std::make_unique<State>()) {
    require(config.mode != Mode::archive || Configuration::valid_date(config.archive_date),400,"Choose a valid archive date");
    state->config=config;
    if (config.mode==Mode::off) return;
    std::filesystem::create_directories(std::filesystem::u8path(directory));
#ifndef _WIN32
    chmod(directory.c_str(),0700);
#endif
    auto pem=directory+"/proxy-ca.pem";
    {
#ifndef _WIN32
    // Android app storage can reject hard links. Serialize identity creation
    // with the same process-safe file locks used for device storage, then
    // publish the complete private file with an atomic rename. Keep the lock
    // file: unlinking it could let different processes lock different inodes.
    struct LockFile { int fd; ~LockFile() { if (fd>=0) close(fd); } } identity_lock{
        open((directory+"/.proxy-ca.lock").c_str(),O_RDWR|O_CREAT|O_NOFOLLOW|O_CLOEXEC,0600)};
    require(identity_lock.fd>=0,503,"Could not lock proxy identity");
    struct stat lock_stat{};
    require(!fstat(identity_lock.fd,&lock_stat) && S_ISREG(lock_stat.st_mode) &&
            lock_stat.st_uid==getuid() && !(lock_stat.st_mode&077),503,"Proxy identity lock permissions are invalid");
    int locked;
    do { locked=flock(identity_lock.fd,LOCK_EX); } while (locked && errno==EINTR);
    require(!locked,503,"Could not lock proxy identity");
#endif
    auto load_identity = [&] {
#ifndef _WIN32
        struct stat st{}; require(!lstat(pem.c_str(),&st) && S_ISREG(st.st_mode) && st.st_uid==getuid() && !(st.st_mode&077),503,"Proxy key permissions are invalid");
#endif
        FILE *f=open_file(pem,"rb"); require(f,503,"Could not read proxy certificate");
        state->key.reset(PEM_read_PrivateKey(f,nullptr,nullptr,nullptr)); state->ca.reset(PEM_read_X509(f,nullptr,nullptr,nullptr)); fclose(f);
        require(state->key && state->ca && X509_check_private_key(state->ca.get(),state->key.get())==1,503,"Saved proxy identity is invalid");
    };
    if (std::filesystem::exists(std::filesystem::u8path(pem))) load_identity();
    else {
        state->key=new_key(); state->ca=make_cert(state->key.get(),state->key.get(),nullptr,"LightTouch Device Proxy");
        // A unique staging name survives interrupted first launches. The lock
        // above protects the existing identity, which guests may already trust.
        unsigned char random[16]; require(RAND_bytes(random,sizeof(random))==1,503,"Random source failed");
        std::string temp=pem+".tmp."; const char hex[]="0123456789abcdef";
        for (auto byte:random) { temp+=hex[byte>>4];temp+=hex[byte&15]; }
        struct Cleanup { std::string path; ~Cleanup() { std::error_code error; std::filesystem::remove(std::filesystem::u8path(path),error); } };
#ifndef _WIN32
        int fd=open(temp.c_str(),O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,0600); require(fd>=0,503,"Could not save proxy key");
        FILE *f=fdopen(fd,"wb"); if (!f) close(fd);
#else
        // msvcrt's fopen has no "x" (create only); _O_EXCL is the same.
        int fd=_wopen(std::filesystem::u8path(temp).c_str(),_O_WRONLY|_O_CREAT|_O_EXCL|_O_BINARY,_S_IREAD|_S_IWRITE);
        require(fd>=0,503,"Could not save proxy key");
        FILE *f=_fdopen(fd,"wb"); if (!f) _close(fd);
#endif
        require(f,503,"Could not save proxy key");
        Cleanup cleanup{temp};
        bool ok=PEM_write_PrivateKey(f,state->key.get(),nullptr,nullptr,0,nullptr,nullptr)==1 && PEM_write_X509(f,state->ca.get())==1;
        ok=fflush(f)==0 && ok;
#ifndef _WIN32
        ok=fsync(fileno(f))==0 && ok;
#endif
        ok=fclose(f)==0 && ok; require(ok,503,"Could not save proxy key");
#ifndef _WIN32
        if (rename(temp.c_str(),pem.c_str())) throw Error(503,std::string("Could not install proxy key: ")+strerror(errno));
#else
        std::error_code error; std::filesystem::rename(std::filesystem::u8path(temp),std::filesystem::u8path(pem),error);
        if (error) { require(std::filesystem::exists(std::filesystem::u8path(pem)),503,"Could not install proxy key");load_identity(); }
#endif
    }
    } // Release the identity lock before preparing per-session TLS state.
    int size=i2d_X509(state->ca.get(),nullptr); require(size>0,503,"Could not encode proxy certificate");
    state->der.resize(size); auto bytes=reinterpret_cast<unsigned char *>(state->der.data()); i2d_X509(state->ca.get(),&bytes);
    std::ofstream(std::filesystem::u8path(directory+"/proxy-ca.der"),std::ios::binary).write(state->der.data(),state->der.size());
    state->leaf_key=new_key();
    state->client.reset(SSL_CTX_new(TLS_client_method())); require(bool(state->client),503,"Host TLS initialization failed");
    SSL_CTX_set_verify(state->client.get(),SSL_VERIFY_PEER,nullptr); SSL_CTX_set_min_proto_version(state->client.get(),TLS1_2_VERSION);
    SSL_CTX_set_default_verify_paths(state->client.get());
    if (!roots.empty()) {
        unsigned loaded=0;
        for (auto &file:std::filesystem::directory_iterator(roots)) {
            FILE *f=fopen(file.path().string().c_str(),"rb"); if (!f) continue;
            X509 *c=PEM_read_X509(f,nullptr,nullptr,nullptr); fclose(f);
            if (c) { if (X509_STORE_add_cert(SSL_CTX_get_cert_store(state->client.get()),c)==1) loaded++; X509_free(c); }
        }
        require(loaded,503,"Android's trusted HTTPS certificates are unavailable");
    }
}
Proxy::~Proxy() {
    state->stopping=true;
    auto fd=state->listener.exchange(invalid_socket);
    if (fd!=invalid_socket) { shutdown(fd,2); close_socket(fd); }
    if (state->acceptor.joinable()) state->acceptor.join();
    std::unique_lock<std::mutex> lock(state->mutex);
    for (auto socket:state->active) shutdown(socket,2);
    state->idle.wait(lock,[&]{return state->active.empty();});
    lock.unlock();
    if (!state->socket_path.empty()) { std::error_code error; std::filesystem::remove(std::filesystem::u8path(state->socket_path),error); }
}
std::string Proxy::certificate() const { return state->der; }
std::string Proxy::fingerprint() const {
    unsigned char digest[EVP_MAX_MD_SIZE]; unsigned length=0;
    if (!state->ca || X509_digest(state->ca.get(),EVP_sha256(),digest,&length)!=1) return "";
    std::string out; const char hex[]="0123456789ABCDEF";
    for (unsigned i=0;i<length;i++) { if (i) out+=':'; out+=hex[digest[i]>>4];out+=hex[digest[i]&15]; }
    return out;
}
std::string Activity::summary() const {
    if (active) return std::to_string(active) + (active==1 ? " web connection in progress" : " web connections in progress");
    return connections ? "Idle — no web connections in progress" : "No web connections received yet";
}
Activity Proxy::activity() const { std::lock_guard<std::mutex> lock(state->mutex); return state->activity; }
std::string Proxy::status() const { return activity().summary(); }
void Proxy::configure(Configuration config) {
    require(config.mode==Mode::off || config.mode==Mode::direct || config.mode==Mode::archive,400,"Invalid network mode");
    require(config.mode!=Mode::archive || Configuration::valid_date(config.archive_date),400,"Choose a valid archive date");
    require(config.mode==Mode::off || bool(state->ca),503,"Enable networking and restart the device first");
    std::lock_guard<std::mutex> lock(state->config_mutex);
    state->config=std::move(config);
}
void Proxy::listen(const std::string &path) {
    require(state->configuration().mode!=Mode::off,503,"Networking is off");
    require(path.size()<sizeof(sockaddr_un::sun_path),503,"Proxy socket path exceeds platform limit");
    Socket fd=socket(AF_UNIX,SOCK_STREAM,0); require(fd!=invalid_socket,503,"Could not create proxy socket");
    sockaddr_un address{}; address.sun_family=AF_UNIX; memcpy(address.sun_path,path.c_str(),path.size()+1);
#ifndef _WIN32
    bool bound=!bind(fd,reinterpret_cast<sockaddr *>(&address),sizeof(address)) && !chmod(path.c_str(),0600) && !::listen(fd,16);
#else
    // Windows 10 1803 and later have AF_UNIX; the socket file takes its folder's ACL (the user's own temp folder).
    bool bound=!bind(fd,reinterpret_cast<sockaddr *>(&address),int(sizeof(address))) && !::listen(fd,16);
#endif
    if (!bound) { close_socket(fd); throw Error(503,"Could not bind private proxy socket"); }
    state->socket_path=path; state->listener=fd;
    state->acceptor=std::thread([this,fd] {
        while (!state->stopping) {
            Socket peer=accept(fd,nullptr,nullptr);
            if (peer==invalid_socket) {
#ifndef _WIN32
                if (errno==EINTR) continue;
#endif
                break;
            }
            timeout(peer,30);
            std::unique_lock<std::mutex> lock(state->mutex);
            if (state->stopping || state->active.size()>=16) { close_socket(peer); continue; }
            state->active.insert(peer); lock.unlock();
            std::thread([this,peer] {
                try { serve(peer); } catch (...) {}
                std::lock_guard<std::mutex> guard(state->mutex);
                close_socket(peer); state->active.erase(peer); state->idle.notify_all();
            }).detach();
        }
    });
}
void Proxy::serve(std::intptr_t socket) {
    [[maybe_unused]] PipeSignal pipe_signal;
#ifdef SO_NOSIGPIPE
    int no_pipe = 1; setsockopt(Socket(socket), SOL_SOCKET, SO_NOSIGPIPE, &no_pipe, sizeof(no_pipe));
#endif
    Connection guest(Socket(socket),false);
    state->begin();
    try {
        require(state->configuration().mode!=Mode::off,503,"Networking is off");
        std::string line=guest.line(); std::istringstream first(line); std::string method,target,version,extra;
        first>>method>>target>>version>>extra;
        require((version=="HTTP/1.0" || version=="HTTP/1.1") && extra.empty(),400,"Invalid HTTP request");
        auto h=headers(guest);
        if (method=="CONNECT") {
            require(!h.count("transfer-encoding") && (!h.count("content-length") || h["content-length"]=="0"),400,"CONNECT cannot have a body");
            auto address=Address::parse("https://"+target+"/");
            require(address.port==443,403,"HTTPS bridge supports port 443");
            Cert leaf=make_cert(state->leaf_key.get(),state->key.get(),state->ca.get(),address.host);
            SSL_CTX *ctx=SSL_CTX_new(TLS_server_method()); require(ctx,503,"Guest TLS initialization failed");
            // Only the guest-facing bridge permits historical protocols and ciphers.
            SSL_CTX_set_security_level(ctx,0); SSL_CTX_set_min_proto_version(ctx,TLS1_VERSION);
            SSL_CTX_set_cipher_list(ctx,"DEFAULT:@SECLEVEL=0");
            int ok=SSL_CTX_use_certificate(ctx,leaf.get()) && SSL_CTX_use_PrivateKey(ctx,state->leaf_key.get());
            guest.tls=SSL_new(ctx); SSL_CTX_free(ctx); require(ok && guest.tls,503,"Guest TLS identity failed");
            // CONNECT acknowledgment must precede TLS bytes.
            SSL *tls=guest.tls; guest.tls=nullptr; guest.write("HTTP/1.1 200 Connection established\r\n\r\n"); guest.tls=tls;
            SSL_set_fd(guest.tls,int(guest.fd));
            if (SSL_accept(guest.tls)!=1) {
                char reason[256]; ERR_error_string_n(ERR_get_error(),reason,sizeof(reason));
                throw Error(502,std::string("Guest TLS handshake failed: ")+reason);
            }
            line=guest.line(); std::istringstream secure(line);
            method.clear(); target.clear(); version.clear(); extra.clear(); secure>>method>>target>>version>>extra;
            require((version=="HTTP/1.0" || version=="HTTP/1.1") && extra.empty(),400,"Invalid HTTP request");
            h=headers(guest);
            require(target.size() && target[0]=='/' && !starts(target,"//"),400,"Invalid HTTPS request target");
            target="https://"+authority(address)+target;
        }
        require(method=="GET" || method=="HEAD" || method=="POST" || method=="PUT" || method=="DELETE" || method=="OPTIONS" || method=="PATCH",405,"Unsupported HTTP method");
        Address address=Address::parse(target);
        const auto config=state->configuration();
        require(config.mode!=Mode::off,503,"Networking is off");
        require(!h.count("expect"),417,"Expect requests are unsupported");
        if (config.mode==Mode::archive) require(method=="GET" || method=="HEAD",405,"Wayback browsing supports GET and HEAD; forms need Direct internet");
        auto upload=body(guest,h,false,upload_limit);
        Reply reply=config.mode==Mode::archive ? state->archived(config,address.url(),method) : state->fetch(config,address.url(),method,h,upload);
        // The guest URL stays the same when the date changes. Keep the dated
        // cache in the host, where its key includes the selected date.
        if (config.mode==Mode::archive) reply.headers["cache-control"]="no-store";
        guest.write(response(reply,method=="HEAD"));
        std::string result = "HTTP " + std::to_string(reply.status);
        if (config.mode==Mode::archive) {
            // Which copy came back: the Archive serves the capture nearest the chosen day when it has none that day.
            const std::string chosen = long_day(config.archive_date), served = reply.capture.empty() ? "" : long_day(reply.capture);
            const std::string error = reply.status>=400 ? " (HTTP " + std::to_string(reply.status) + ")" : "";
            if (reply.status==429 || reply.status==503) result = "Internet Archive is busy. Try reloading later.";
            else if (served.empty()) { if (reply.status==404) result = "No archived capture is available for this page"; }
            else if (reply.capture==config.archive_date) result = (error.empty() ? "Loaded the copy from " : "The copy from ") + served + (error.empty() ? "" : " is an error page" + error);
            else result = "No copy from " + chosen + (error.empty() ? "; loaded the nearest, from " + served : "; the nearest, from " + served + ", is an error page" + error);
        }
        state->finish(result, reply.status>=400);
    } catch (const std::exception &error) {
        try { auto typed=dynamic_cast<const Error *>(&error); guest.write(response({typed?typed->status:502,{{"content-type","text/plain; charset=utf-8"}},std::string(error.what())+"\n"})); } catch (...) {}
        state->finish(error.what(), true);
    }
}
} // namespace lt::network
