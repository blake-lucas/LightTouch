// Real production proxy/TLS/cache, with DNS and connect redirected only in this
// Linux fixture. No Internet Archive traffic or production injection hooks.
#include "../../network/Network.cpp"
#include <cassert>
#include <future>
#include <iostream>
using namespace lt::network;

static std::atomic<unsigned short> archive_port{0}, direct_port{0};
extern "C" int __real_getaddrinfo(const char *, const char *, const addrinfo *, addrinfo **);
extern "C" int __wrap_getaddrinfo(const char *host, const char *service, const addrinfo *hints, addrinfo **out) {
    assert(!strcmp(host,"web.archive.org") || !strcmp(host,"origin.test"));
    return __real_getaddrinfo("93.184.216.34",service,hints,out);
}
extern "C" int __real_connect(int, const sockaddr *, socklen_t);
extern "C" int __wrap_connect(int fd, const sockaddr *address, socklen_t size) {
    assert(address->sa_family==AF_INET);
    auto local=*reinterpret_cast<const sockaddr_in *>(address);
    local.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    local.sin_port=htons(ntohs(local.sin_port)==443?archive_port.load():direct_port.load());
    return __real_connect(fd,reinterpret_cast<sockaddr *>(&local),size);
}

static int listener(std::atomic<unsigned short> &port) {
    int fd=socket(AF_INET,SOCK_STREAM,0);
    assert(fd>=0);
    sockaddr_in address{};address.sin_family=AF_INET;address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    assert(!bind(fd,reinterpret_cast<sockaddr *>(&address),sizeof(address)));
    assert(!::listen(fd,8));
    socklen_t size=sizeof(address);
    assert(!getsockname(fd,reinterpret_cast<sockaddr *>(&address),&size));
    port=ntohs(address.sin_port);
    return fd;
}
static std::string request(Proxy &proxy, const std::string &path, const std::string &method="GET") {
    int pair[2];assert(!socketpair(AF_UNIX,SOCK_STREAM,0,pair));
    std::thread worker([&]{proxy.serve(pair[1]);close(pair[1]);});
    Connection guest(pair[0]);
    guest.write(method+" http://origin.test/"+path+" HTTP/1.1\r\nHost: origin.test\r\n\r\n");
    std::string out;char block[4096];int count;
    while((count=guest.read(block,sizeof(block)))>0) out.append(block,count);
    worker.join();
    return out;
}
int main() {
    signal(SIGPIPE,SIG_IGN);
    char tmp[]="/tmp/lt-network-live-XXXXXX";assert(mkdtemp(tmp));
    std::string root=tmp;
    std::filesystem::create_directory(root+"/roots");
    auto key=new_key();
    auto ca=make_cert(key.get(),key.get(),nullptr,"Fixture root");
    assert(X509_sign(ca.get(),key.get(),EVP_sha256())>0);
    auto leaf=make_cert(key.get(),key.get(),ca.get(),"web.archive.org");
    // make_cert produces SHA-1 guest leaves. This fixture impersonates the
    // modern upstream and must pass the host's unchanged TLS security policy.
    assert(X509_sign(leaf.get(),key.get(),EVP_sha256())>0);
    FILE *pem=fopen((root+"/roots/root.pem").c_str(),"wb");assert(pem);
    assert(PEM_write_X509(pem,ca.get())==1);fclose(pem);
    SSL_CTX *tls=SSL_CTX_new(TLS_server_method());assert(tls);
    assert(SSL_CTX_use_certificate(tls,leaf.get())==1 && SSL_CTX_use_PrivateKey(tls,key.get())==1);
    int archived=listener(archive_port), direct=listener(direct_port);
    std::promise<void> slow_entered, release_slow;
    auto released=release_slow.get_future();
    std::atomic<unsigned> archive_requests{0};
    std::thread archive_server([&] {
        for (;;) {
            int fd=accept(archived,nullptr,nullptr);if(fd<0) break;
            Connection peer(fd);timeout(fd,5);
            peer.tls=SSL_new(tls);SSL_set_fd(peer.tls,fd);assert(SSL_accept(peer.tls)==1);
            auto line=peer.line();(void)headers(peer);++archive_requests;
            // The Archive's day-only URLs redirect to the capture it serves; /gone has none at all.
            auto has=[&](const char *text){return line.find(text)!=std::string::npos;};
            if(has("/gone ")) { peer.write(response({404,{{"content-type","text/plain"}},"not archived"})); continue; }
            if(has("20090909id_/") && (has("/exact ") || has("/nearest ") || has("/missing "))) {
                auto path=line.substr(line.find("id_/")+4);path=path.substr(0,path.find(' '));
                auto stamp=has("/exact ")?"20090909120000":"20090912081500";
                peer.write(response({302,{{"location",std::string("/web/")+stamp+"id_/"+path}},""}));continue;
            }
            if(has("20090912081500id_/http://origin.test/missing ")) { peer.write(response({404,{{"content-type","text/plain"}},"captured 404"})); continue; }
            auto body=line.find("20090909id_")!=std::string::npos?"20090909":"20100101";
            if(line.find("/slow ")!=std::string::npos && std::string(body)=="20090909") {
                slow_entered.set_value();released.wait();
            }
            peer.write(response({200,{{"content-type","text/plain"}},body}));
        }
    });
    std::thread direct_server([&] {
        for (;;) {
            int fd=accept(direct,nullptr,nullptr);if(fd<0) break;
            Connection peer(fd);timeout(fd,5);(void)peer.line();(void)headers(peer);
            peer.write(response({200,{{"content-type","text/plain"}},"live"}));
        }
    });
    {
        Configuration config;config.mode=Mode::archive;
        Proxy proxy(root+"/proxy",config,root+"/roots");
        auto fingerprint=proxy.fingerprint();
        auto first=request(proxy,"page");
        assert(first.find("20090909")!=std::string::npos && first.find("cache-control: no-store")!=std::string::npos);
        assert(request(proxy,"page").find("20090909")!=std::string::npos && archive_requests==1);
        auto old=std::async(std::launch::async,[&]{return request(proxy,"slow");});
        assert(slow_entered.get_future().wait_for(5s)==std::future_status::ready);
        // This must finish with the archive fetch deliberately blocked. A
        // timeout also detects accidentally sharing its gate with configure().
        auto live=std::async(std::launch::async,[&] {
            auto next=config;next.mode=Mode::direct;proxy.configure(next);
            return request(proxy,"page");
        });
        assert(live.wait_for(2s)==std::future_status::ready);
        assert(live.get().find("live")!=std::string::npos);
        config.archive_date="20100101";proxy.configure(config);
        release_slow.set_value();
        assert(old.get().find("20090909")!=std::string::npos);
        assert(request(proxy,"page").find("20100101")!=std::string::npos);
        // The older in-flight completion must not poison the new date's cache.
        assert(request(proxy,"slow").find("20100101")!=std::string::npos);
        assert(archive_requests==4);
        config.archive_date="20090909";proxy.configure(config);
        assert(request(proxy,"slow").find("20090909")!=std::string::npos && archive_requests==4);
        assert(request(proxy,"login","POST").find("405")!=std::string::npos);
        // Current session names the copy that came back: the chosen day's, or the nearest one.
        assert(request(proxy,"exact").find("HTTP/1.1 200 ")==0);
        assert(proxy.activity().last_result=="Loaded the copy from 9 September 2009");
        request(proxy,"nearest");
        assert(proxy.activity().last_result=="No copy from 9 September 2009; loaded the nearest, from 12 September 2009");
        assert(!proxy.activity().last_result_error);
        request(proxy,"missing");
        assert(proxy.activity().last_result=="No copy from 9 September 2009; the nearest, from 12 September 2009, is an error page (HTTP 404)");
        assert(proxy.activity().last_result_error);
        request(proxy,"gone");
        assert(proxy.activity().last_result=="No archived capture is available for this page");
        config.mode=Mode::off;proxy.configure(config);
        assert(request(proxy,"page").find("503")!=std::string::npos);
        config.mode=Mode::direct;proxy.configure(config);
        assert(request(proxy,"page").find("live")!=std::string::npos);
        auto bad=config;bad.mode=Mode::archive;bad.archive_date="20090230";
        bool refused=false;try {proxy.configure(bad);} catch (...) {refused=true;}
        assert(refused && request(proxy,"page").find("live")!=std::string::npos);
        assert(proxy.fingerprint()==fingerprint);
    }
    shutdown(archived,SHUT_RDWR);shutdown(direct,SHUT_RDWR);
    archive_server.join();direct_server.join();close(archived);close(direct);SSL_CTX_free(tls);
    std::filesystem::remove_all(root);
    std::cout<<"PASS: live routes, dates, in-flight snapshots, date cache isolation, nonblocking changes, off/on, persistent identity\n";
}
