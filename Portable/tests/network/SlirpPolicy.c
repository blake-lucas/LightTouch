/* Exercise the actual vendored NAT translation, including the explicit DNS
 * exemption, and restricted mode's immediate DNS refusal. This fixture performs
 * no outbound connections. */
#include "slirp.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

static bool route(Slirp *slirp, const char *target, bool dns) {
    struct socket so = {0};
    struct sockaddr_storage address = {0};
    so.slirp = slirp;
    if (strchr(target, ':')) {
        address.ss_family = AF_INET6;
        assert(inet_pton(AF_INET6, target, &((struct sockaddr_in6 *)&address)->sin6_addr) == 1);
        so.so_ffamily = AF_INET6;
        so.so_faddr6 = ((struct sockaddr_in6 *)&address)->sin6_addr;
    } else {
        address.ss_family = AF_INET;
        assert(inet_pton(AF_INET, target, &((struct sockaddr_in *)&address)->sin_addr) == 1);
        so.so_ffamily = AF_INET;
        so.so_faddr = ((struct sockaddr_in *)&address)->sin_addr;
    }
    so.so_type = dns ? IPPROTO_UDP : IPPROTO_TCP;
    so.so_fport = htons(dns ? 53 : 80);
    return sotranslate_out(&so, &address) == 0;
}
static uint8_t sent[1600];
static int sent_len, sent_count;
static slirp_ssize_t capture(const void *buf, size_t len, void *opaque) {
    (void)opaque;
    assert(len <= sizeof(sent));
    memcpy(sent, buf, len);
    sent_len = (int)len;
    sent_count++;
    return (slirp_ssize_t)len;
}
static void guest_error(const char *msg, void *opaque) { (void)opaque; fprintf(stderr, "guest error: %s\n", msg); }
static int64_t clock_ns(void *opaque) { (void)opaque; return g_get_monotonic_time() * 1000; }
static void notify(void *opaque) { (void)opaque; }
static const uint8_t guest_mac[6] = {0x52, 0x55, 0x0a, 0x00, 0x02, 0x0f};
static const uint8_t query[] = {
    0xbe, 0xef, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0,
    7, 'e', 'x', 'a', 'm', 'p', 'l', 'e', 3, 'c', 'o', 'm', 0, 0, 1, 0, 1};

/* A guest UDP query from 10.0.2.15:40000 to `server`:53, as an Ethernet frame. */
static int dns_frame(uint8_t *f, const char *server) {
    const int udp_len = 8 + (int)sizeof(query), ip_len = 20 + udp_len;
    memset(f, 0, 14 + ip_len);
    memset(f, 0x52, 1); f[1] = 0x55; f[2] = 0x0a; f[3] = 0; f[4] = 2; f[5] = 2;
    memcpy(f + 6, guest_mac, 6);
    f[12] = 0x08; f[13] = 0x00;
    uint8_t *ip = f + 14;
    ip[0] = 0x45; ip[2] = ip_len >> 8; ip[3] = ip_len & 0xff; ip[8] = 64; ip[9] = 17;
    assert(inet_pton(AF_INET, "10.0.2.15", ip + 12) == 1);
    assert(inet_pton(AF_INET, server, ip + 16) == 1);
    uint32_t sum = 0;
    for (int i = 0; i < 20; i += 2) sum += (ip[i] << 8) | ip[i + 1];
    while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
    ip[10] = ~sum >> 8; ip[11] = ~sum & 0xff;
    uint8_t *udp = ip + 20;
    udp[0] = 40000 >> 8; udp[1] = 40000 & 0xff; udp[3] = 53;
    udp[4] = udp_len >> 8; udp[5] = udp_len & 0xff; /* checksum 0: none */
    memcpy(udp + 8, query, sizeof(query));
    return 14 + ip_len;
}

/* Networking off: a lookup sent to the virtual nameserver is answered at once
 * with NXDOMAIN (and a zero-TTL SOA), not dropped; other UDP stays dropped. */
static void restricted_dns(void) {
    SlirpConfig cfg = {0};
    cfg.version = 1;
    cfg.restricted = 1;
    cfg.in_enabled = true;
    assert(inet_pton(AF_INET, "10.0.2.0", &cfg.vnetwork) == 1);
    assert(inet_pton(AF_INET, "255.255.255.0", &cfg.vnetmask) == 1);
    assert(inet_pton(AF_INET, "10.0.2.2", &cfg.vhost) == 1);
    assert(inet_pton(AF_INET, "10.0.2.15", &cfg.vdhcp_start) == 1);
    assert(inet_pton(AF_INET, "10.0.2.3", &cfg.vnameserver) == 1);
    SlirpCb cb = {0};
    cb.send_packet = capture;
    cb.guest_error = guest_error;
    cb.clock_get_ns = clock_ns;
    cb.notify = notify;
    Slirp *s = slirp_new(&cfg, &cb, NULL);
    assert(s);

    /* The guest asks for the gateway first, so slirp knows where to reply. */
    uint8_t arp[42] = {0};
    memset(arp, 0xff, 6);
    memcpy(arp + 6, guest_mac, 6);
    arp[12] = 0x08; arp[13] = 0x06; arp[15] = 1; arp[16] = 0x08; arp[18] = 6; arp[19] = 4; arp[21] = 1;
    memcpy(arp + 22, guest_mac, 6);
    assert(inet_pton(AF_INET, "10.0.2.15", arp + 28) == 1);
    assert(inet_pton(AF_INET, "10.0.2.2", arp + 38) == 1);
    slirp_input(s, arp, sizeof(arp));
    assert(sent_count == 1);

    uint8_t frame[128];
    slirp_input(s, frame, dns_frame(frame, "10.0.2.3"));
    assert(sent_count == 2);
    const uint8_t *ip = sent + 14, *udp = ip + 20, *dns = udp + 8;
    assert(sent[12] == 0x08 && sent[13] == 0x00 && ip[9] == 17);
    assert(memcmp(ip + 12, frame + 14 + 16, 4) == 0 && memcmp(ip + 16, frame + 14 + 12, 4) == 0);
    assert(udp[0] == 0 && udp[1] == 53 && udp[2] == (40000 >> 8) && udp[3] == (40000 & 0xff));
    assert(dns[0] == 0xbe && dns[1] == 0xef);
    assert(dns[2] == 0x81 && dns[3] == 0x83); /* response, RD, RA, NXDOMAIN */
    assert(dns[5] == 1 && dns[7] == 0 && dns[9] == 1 && dns[11] == 0);
    assert(memcmp(dns + 12, query + 12, sizeof(query) - 12) == 0);
    const uint8_t *soa = dns + sizeof(query);
    assert(soa[0] == 0 && soa[2] == 6 && soa[4] == 1);
    assert(soa[5] == 0 && soa[6] == 0 && soa[7] == 0 && soa[8] == 0); /* TTL 0 */
    assert(sent_len - 14 - 20 - 8 == (int)sizeof(query) + 33);

    /* Any other destination is still dropped without a word. */
    slirp_input(s, frame, dns_frame(frame, "8.8.8.8"));
    assert(sent_count == 2);
    slirp_cleanup(s);
}

int main(void) {
    restricted_dns();
    Slirp slirp = {0};
    assert(inet_pton(AF_INET, "10.0.2.2", &slirp.vhost_addr) == 1);
    assert(inet_pton(AF_INET, "10.0.2.3", &slirp.vnameserver_addr) == 1);
    assert(inet_pton(AF_INET6, "fec0::", &slirp.vprefix_addr6) == 1);
    slirp.vprefix_len = 64;
    struct sockaddr_in resolver = {0};
    resolver.sin_family = AF_INET;
    resolver.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    resolver.sin_port = htons(5353);
    slirp_set_dns_forward(&slirp, &resolver);
    for (int local = 0; local <= 1; local++) {
        slirp_set_local_network(&slirp, local);
        assert(route(&slirp, "93.184.216.34", false));
        assert(route(&slirp, "2606:2800:220:1:248:1893:25c8:1946", false));
        assert(route(&slirp, "10.0.2.3", true));
        assert(!route(&slirp, "10.0.2.3", false));
        assert(!route(&slirp, "10.0.2.2", false));
        assert(!route(&slirp, "127.0.0.1", false));
        assert(!route(&slirp, "0.0.0.1", false));
        assert(!route(&slirp, "::1", false));
        assert(!route(&slirp, "::ffff:127.0.0.1", false));
        assert(!route(&slirp, "64:ff9b::7f00:1", false));
        assert(!route(&slirp, "::ffff:0:127.0.0.1", false));
        for (size_t i = 0; i < 3; i++) {
            const char *public4[] = {"::ffff:93.184.216.34", "64:ff9b::5db8:d822", "::ffff:0:93.184.216.34"};
            assert(route(&slirp, public4[i], false));
        }
        const char *private[] = {
            "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.1.1",
            "100.64.0.1", "198.18.0.1", "240.0.0.1", "fd00::1", "fed0::1",
            "::ffff:192.168.1.1", "64:ff9b::c0a8:101", "::ffff:0:192.168.1.1"
        };
        for (size_t i = 0; i < sizeof(private) / sizeof(private[0]); i++) {
            assert(route(&slirp, private[i], false) == !!local);
        }
    }
    puts("PASS: actual slirp NAT LAN policy, host-loopback isolation, Android DNS forwarding and restricted DNS refusal");
}
