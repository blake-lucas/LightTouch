#include "Network.hpp"
#include <openssl/ssl.h>
#include <openssl/pem.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <unistd.h>
#include <ifaddrs.h>
#include <signal.h>
#include <openssl/err.h>
#include <atomic>
#include <cassert>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <thread>
#include <cerrno>
#include <sys/stat.h>
#include <chrono>
using namespace lt::network;
#ifdef LT_TEST_NO_HARD_LINKS
extern "C" int __wrap_link(const char *, const char *) { errno=EPERM;return -1; }
extern "C" int __wrap_linkat(int, const char *, int, const char *, int) { errno=EPERM;return -1; }
#endif
std::string exchange(Proxy &proxy, const std::string &wire, bool tls = false) {
    int sockets[2]; assert(!socketpair(AF_UNIX,SOCK_STREAM,0,sockets));
    std::thread worker([&]{proxy.serve(sockets[1]);close(sockets[1]);});
    SSL_CTX *context=nullptr; SSL *ssl=nullptr;
    if(tls) {
        const std::string connect="CONNECT example.com:443 HTTP/1.1\r\nHost: example.com\r\n\r\n";
        assert(send(sockets[0],connect.data(),connect.size(),0)==ssize_t(connect.size()));
        std::string head;char byte;while(head.find("\r\n\r\n")==std::string::npos){assert(recv(sockets[0],&byte,1,0)==1);head+=byte;}
        assert(head.find("200 Connection established")!=std::string::npos);
        context=SSL_CTX_new(TLS_client_method());SSL_CTX_set_security_level(context,0);
        // The actual guest protocol, not the host's modern TLS default.
        SSL_CTX_set_min_proto_version(context,TLS1_VERSION);SSL_CTX_set_max_proto_version(context,TLS1_VERSION);
        SSL_CTX_set_verify(context,SSL_VERIFY_PEER,nullptr);
        auto der=proxy.certificate();auto p=reinterpret_cast<const unsigned char *>(der.data());
        X509 *ca=d2i_X509(nullptr,&p,der.size());assert(ca);assert(X509_STORE_add_cert(SSL_CTX_get_cert_store(context),ca)==1);X509_free(ca);
        ssl=SSL_new(context);SSL_set_fd(ssl,sockets[0]);SSL_set1_host(ssl,"example.com");
        int result=SSL_connect(ssl);
        if(result!=1){ERR_print_errors_fp(stderr);std::cerr<<proxy.status()<<"\n";}
        assert(result==1);assert(SSL_get_verify_result(ssl)==X509_V_OK);
        assert(SSL_write(ssl,wire.data(),wire.size())==int(wire.size()));
    } else assert(send(sockets[0],wire.data(),wire.size(),0)==ssize_t(wire.size()));
    std::string response;char block[8192];int size;
    while((size=ssl?SSL_read(ssl,block,sizeof(block)):int(recv(sockets[0],block,sizeof(block),0)))>0)response.append(block,size);
    if(ssl)SSL_free(ssl);if(context)SSL_CTX_free(context);
    close(sockets[0]);worker.join();return response;
}
std::string local_address() {
    ifaddrs *list=nullptr;assert(!getifaddrs(&list));std::string address;
    for(auto p=list;p;p=p->ifa_next)if(p->ifa_addr && p->ifa_addr->sa_family==AF_INET){auto a=reinterpret_cast<sockaddr_in *>(p->ifa_addr);if((ntohl(a->sin_addr.s_addr)>>24)!=127){char text[INET_ADDRSTRLEN];inet_ntop(AF_INET,&a->sin_addr,text,sizeof(text));address=text;break;}}
    freeifaddrs(list);return address;
}
int main(int argc,char **argv) {
    signal(SIGPIPE,SIG_IGN);
    if(argc==6 && std::string(argv[1])=="--serve") {
        Configuration c;c.mode=std::string(argv[3])=="archive"?Mode::archive:Mode::direct;c.archive_date=argv[4];
        Proxy proxy(argv[2],c);proxy.listen(argv[5]);std::cout<<"ready\n"<<std::flush;for(;;)pause();
    }
    assert(Configuration::valid_date("20000229"));assert(!Configuration::valid_date("20010229"));
    assert(!Configuration::valid_date("20090230"));assert(!Configuration::valid_date("19951231"));
    assert(Address::parse("https://example.com:8443/a?b=c#d").url()=="https://example.com:8443/a?b=c");
    for(auto bad:{"http://user:pass@example.com/","file:///tmp/a","http://127.0.0.1%23.example.com/","http://a/\r\nx: y"}){
        bool refused=false;try{Address::parse(bad);}catch(...){refused=true;}assert(refused);
    }
    assert(archive_url("http://example.com/","20090909")=="https://web.archive.org/web/20090909id_/http://example.com/");
    assert(archive_redirect("https://web.archive.org/web/20090909id_/http://example.com/a","https://example.com/b","20090909")=="https://web.archive.org/web/20090909id_/https://example.com/b");
    assert(archive_redirect("https://web.archive.org/web/20090909id_/http://example.com/a","/web/20090910080339id_/http://example.com/","20090909")=="https://web.archive.org/web/20090910080339id_/http://example.com/");
    for(auto text:{"127.0.0.1","10.0.0.1","172.16.0.1","192.168.1.1","169.254.169.254","100.64.0.1","::1","::ffff:127.0.0.1","64:ff9b::7f00:1","fc00::1"}) {
        unsigned char ip[16];int family=strchr(text,':')?AF_INET6:AF_INET;assert(inet_pton(family,text,ip)==1);assert(!public_address(ip,family==AF_INET?4:16));
    }
    char tmp[]="/tmp/lt-network-tests-XXXXXX";assert(mkdtemp(tmp));std::string directory=tmp;
    {
        Proxy off(directory+"/off",{});
        assert(exchange(off,"GET http://example.com/ HTTP/1.1\r\n\r\n").find("503")!=std::string::npos);
        assert(!std::filesystem::exists(directory+"/off/proxy-ca.pem"));
        Configuration enabled;enabled.mode=Mode::direct;
        bool refused=false;try{off.configure(enabled);}catch(...){refused=true;}
        assert(refused && off.certificate().empty());
    }
    Configuration config;config.mode=Mode::direct;std::string fingerprint;
    {
        Proxy proxy(directory+"/activity",config);
        assert(proxy.activity().connections==0);
        assert(proxy.status()=="No web connections received yet");
        // Ingress must become visible before any HTTP bytes arrive. Keep a
        // second connection open while the first finishes to check concurrency.
        int first[2],second[2];
        assert(!socketpair(AF_UNIX,SOCK_STREAM,0,first));assert(!socketpair(AF_UNIX,SOCK_STREAM,0,second));
        std::thread a([&]{proxy.serve(first[1]);close(first[1]);});
        std::thread b([&]{proxy.serve(second[1]);close(second[1]);});
        auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(3);
        while(proxy.activity().active!=2 && std::chrono::steady_clock::now()<deadline)std::this_thread::yield();
        auto initial=proxy.activity();
        assert(initial.connections==2 && initial.active==2 && initial.completed==0 && initial.failed==0);
        assert(initial.summary()=="2 web connections in progress");
        const std::string invalid="GET http://127.0.0.1/ HTTP/1.1\r\n\r\n";
        assert(send(first[0],invalid.data(),invalid.size(),0)==ssize_t(invalid.size()));a.join();close(first[0]);
        auto partial=proxy.activity();
        assert(partial.active==1 && partial.completed==1 && partial.failed==1);
        assert(partial.last_result_error && !partial.last_result.empty());
        assert(partial.summary()=="1 web connection in progress");
        shutdown(second[0],SHUT_WR);b.join();close(second[0]);
        auto done=proxy.activity();
        assert(done.connections==2 && done.active==0 && done.completed==2 && done.failed==2);
        assert(done.summary()=="Idle — no web connections in progress");
    }
    {
        // A killed first launch can leave an incomplete staging file. The next
        // launch must recover, then keep the same identity across later boots.
        auto recovery=directory+"/recovery";
        std::filesystem::create_directory(recovery);
        std::ofstream(recovery+"/proxy-ca.pem.tmp")<<"incomplete private key";
        std::string identity;
        { Proxy proxy(recovery,config);identity=proxy.fingerprint();assert(!identity.empty()); }
        { Proxy proxy(recovery,config);assert(proxy.fingerprint()==identity); }
        struct stat saved{};assert(!stat((recovery+"/proxy-ca.pem").c_str(),&saved));
        assert((saved.st_mode&0777)==0600);
    }
    {
        // Concurrent first starts must settle on one persistent CA even on
        // filesystems/platforms that deny link/linkat (the Linux fixture does).
        std::string identities[2];std::atomic<int> ready{0};
        auto start=[&](int index) {
            ready++;while(ready.load()<2)std::this_thread::yield();
            Proxy proxy(directory+"/concurrent",config);identities[index]=proxy.fingerprint();
        };
        std::thread first(start,0),second(start,1);first.join();second.join();
        assert(!identities[0].empty() && identities[0]==identities[1]);
        Proxy reopened(directory+"/concurrent",config);assert(reopened.fingerprint()==identities[0]);
    }
    {
        Proxy direct(directory+"/direct",config);fingerprint=direct.fingerprint();assert(!fingerprint.empty());
        assert(exchange(direct,"GET http://127.0.0.1/ HTTP/1.1\r\n\r\n").find("403")!=std::string::npos);
        assert(exchange(direct,"GET http://[::ffff:127.0.0.1]/ HTTP/1.1\r\n\r\n").find("403")!=std::string::npos);
        assert(exchange(direct,"POST http://example.com/ HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n").find("400")!=std::string::npos);
        assert(exchange(direct,"POST http://example.com/ HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n").find("400")!=std::string::npos);
        auto address=local_address();assert(!address.empty());
        assert(exchange(direct,"GET http://"+address+"/ HTTP/1.1\r\n\r\n").find("403")!=std::string::npos);
    }
    {
        config.local_network=true;Proxy direct(directory+"/direct",config);assert(direct.fingerprint()==fingerprint);
        // Real HTTP over a resolved, admitted LAN socket; decode a chunked reply
        // and strip proxy credentials and Connection-nominated headers.
        int listener=socket(AF_INET,SOCK_STREAM,0);assert(listener>=0);
        sockaddr_in a{};a.sin_family=AF_INET;a.sin_addr.s_addr=htonl(INADDR_ANY);assert(!bind(listener,reinterpret_cast<sockaddr *>(&a),sizeof(a)));assert(!listen(listener,1));
        socklen_t len=sizeof(a);assert(!getsockname(listener,reinterpret_cast<sockaddr *>(&a),&len));
        for (bool drop_cookie:{false,true}) {
        std::string observed;
        std::thread origin([&]{int peer=accept(listener,nullptr,nullptr);assert(peer>=0);char c;while(observed.find("\r\n\r\n")==std::string::npos){assert(recv(peer,&c,1,0)==1);observed+=c;}
            std::string r=std::string("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close")+(drop_cookie?", Set-Cookie":"")+"\r\nSet-Cookie: first=1; Path=/\r\nSet-Cookie: second=2; Path=/\r\nContent-Type: text/plain\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
            assert(send(peer,r.data(),r.size(),0)==ssize_t(r.size()));close(peer);});
        auto r=exchange(direct,"GET http://"+local_address()+":"+std::to_string(ntohs(a.sin_port))+"/ HTTP/1.1\r\nProxy-Authorization: secret\r\nConnection: X-Drop\r\nX-Drop: hidden\r\n\r\n");origin.join();
        assert(r.find("200")!=std::string::npos && r.substr(r.size()-5)=="hello");assert(r.find("Content-Length: 5")!=std::string::npos);
        assert(observed.find("secret")==std::string::npos && observed.find("hidden")==std::string::npos);
        assert((r.find("Set-Cookie: first=1; Path=/\r\n")!=std::string::npos)==!drop_cookie);
        assert((r.find("Set-Cookie: second=2; Path=/\r\n")!=std::string::npos)==!drop_cookie);
        auto activity=direct.activity();
        assert(activity.completed==unsigned(drop_cookie?2:1) && activity.failed==0 && activity.active==0);
        assert(activity.last_result=="HTTP 200" && !activity.last_result_error);
        }
        close(listener);
        // Host loopback remains refused even after LAN opt-in.
        assert(exchange(direct,"GET http://127.1/ HTTP/1.1\r\n\r\n").find("403")!=std::string::npos);
        assert(exchange(direct,"GET http://[::ffff:127.0.0.1]/ HTTP/1.1\r\n\r\n").find("403")!=std::string::npos);
    }
    {
        config.mode=Mode::archive;config.archive_date="20090909";Proxy archive(directory+"/archive",config);
        auto r=exchange(archive,"POST /login HTTP/1.1\r\nHost: example.com\r\nContent-Length: 0\r\n\r\n",true);
        assert(r.find("405")!=std::string::npos && r.find("Wayback browsing")!=std::string::npos);
        for (auto line:{"POST /login HTTP/2.0", "POST /login HTTP/1.1 unexpected", "POST /login"}) {
            assert(exchange(archive,std::string(line)+"\r\nHost: example.com\r\n\r\n",true).find("400")!=std::string::npos);
        }
        assert(exchange(archive,"CONNECT example.com:22 HTTP/1.1\r\n\r\n").find("403")!=std::string::npos);
    }
    std::filesystem::remove_all(directory);
    std::cout<<"PASS: routing, dates, address policy, framing, offline, LAN opt-in, chunked HTTP, credentials, persistent CA, verified TLS 1.0 bridge, archive method isolation\n";
}
