// One IPv4 station and one HTTPS socket. Every IP packet goes over Moto AXI.
#include "lwip/dhcp.h"
#include "lwip/dns.h"
#include "lwip/etharp.h"
#include "lwip/init.h"
#include "lwip/netif.h"
#include "lwip/tcp.h"
#include "lwip/timeouts.h"
#include "netif/ethernet.h"
#include <string.h>
extern int radio_l2_send(const unsigned char *, size_t),
    radio_l2_recv(unsigned char *, size_t);
extern int radio_random(unsigned char *, size_t), guest_https(unsigned char *);
extern unsigned radio_monotonic_ms(void);
static struct netif iface;
// Access-point mode: VioletOS hosts 192.168.77.0/24 and relays TCP through the phone.
#define HOST_ADDR 0xc0a84d01u  /* 192.168.77.1 */
#define PHONE_ADDR 0xc0a84d02u /* 192.168.77.2 */
static int hosting, leased;
static unsigned char host_mac[6], relay_password[64];
static unsigned relay_password_len;

static struct tcp_pcb *connection;
static struct pbuf *pending;
static unsigned request_start;
static int established, failed, eof, initialized, attached, dns_state;
static ip_addr_t destination;
static unsigned char frame[1518];
// The name sent with DHCP requests. Empty by default: no name is sent.
static char hostname[33];
u32_t sys_now(void) { return radio_monotonic_ms(); }
unsigned radio_ip_random(void) {
  unsigned v = 0;
  if (radio_random((unsigned char *)&v, sizeof(v)))
    failed = 1;
  return v;
}
int radio_entropy(unsigned char seed[32]) {
  return radio_random(seed, 32) == 0;
}
static err_t send_frame(struct netif *n, struct pbuf *packet) {
  if (packet->tot_len > sizeof(frame))
    return ERR_BUF;
  u16_t len = pbuf_copy_partial(packet, frame, packet->tot_len, 0);
  return len == packet->tot_len && radio_l2_send(frame, len) == 0 ? ERR_OK
                                                                  : ERR_IF;
}
static err_t setup(struct netif *n) {
  n->name[0] = 'w';
  n->name[1] = 'l';
  n->mtu = 1500;
  n->hwaddr_len = 6;
  n->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP;
  n->output = etharp_output;
  n->linkoutput = send_frame;
  return ERR_OK;
}
static unsigned short ip_checksum(const unsigned char *p, unsigned n) {
  unsigned sum = 0;
  for (unsigned i = 0; i + 1 < n; i += 2)
    sum += (unsigned)p[i] << 8 | p[i + 1];
  while (sum >> 16)
    sum = (sum & 0xffff) + (sum >> 16);
  return (unsigned short)~sum;
}
static void put32(unsigned char *p, unsigned v) {
  p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v;
}
// Answer DHCPDISCOVER and DHCPREQUEST from the phone with one fixed lease.
// No router or DNS option: the phone keeps its default route on cellular.
static int serve_dhcp(const unsigned char *f, int len) {
  if (len < 14 + 20 + 8 + 240 || f[12] != 8 || f[13] != 0 || (f[14] >> 4) != 4)
    return 0;
  unsigned ihl = (f[14] & 15) * 4;
  const unsigned char *udp = f + 14 + ihl;
  if (f[23] != 17 || 14 + ihl + 8 + 240 > (unsigned)len || udp[2] != 0 || udp[3] != 67)
    return 0;
  const unsigned char *bootp = udp + 8;
  unsigned blen = len - (14 + ihl + 8);
  if (bootp[0] != 1 || bootp[1] != 1 || bootp[2] != 6 ||
      bootp[236] != 99 || bootp[237] != 130 || bootp[238] != 83 || bootp[239] != 99)
    return 1;
  int type = 0;
  for (unsigned i = 240; i + 1 < blen && bootp[i] != 255;) {
    if (bootp[i] == 0) { i++; continue; }
    if (bootp[i] == 53 && bootp[i + 1] == 1 && i + 2 < blen)
      type = bootp[i + 2];
    i += 2 + bootp[i + 1];
  }
  if (type != 1 && type != 3)
    return 1;
  static unsigned char r[14 + 20 + 8 + 300];
  memset(r, 0, sizeof(r));
  memcpy(r, bootp + 28, 6);
  memcpy(r + 6, host_mac, 6);
  r[12] = 8;
  unsigned char *ip = r + 14, *u = ip + 20, *b = u + 8;
  b[0] = 2; b[1] = 1; b[2] = 6;
  memcpy(b + 4, bootp + 4, 4);   /* xid */
  memcpy(b + 10, bootp + 10, 2); /* flags */
  put32(b + 16, PHONE_ADDR);
  put32(b + 20, HOST_ADDR);
  memcpy(b + 28, bootp + 28, 16);
  b[236] = 99; b[237] = 130; b[238] = 83; b[239] = 99;
  unsigned char *o = b + 240;
  *o++ = 53; *o++ = 1; *o++ = type == 1 ? 2 : 5;
  *o++ = 54; *o++ = 4; put32(o, HOST_ADDR); o += 4;
  *o++ = 51; *o++ = 4; put32(o, 86400); o += 4;
  *o++ = 1; *o++ = 4; put32(o, 0xffffff00u); o += 4;
  *o++ = 255;
  unsigned ulen = 8 + 300, total = 20 + ulen;
  ip[0] = 0x45; ip[2] = total >> 8; ip[3] = total; ip[8] = 64; ip[9] = 17;
  put32(ip + 12, HOST_ADDR);
  put32(ip + 16, 0xffffffffu);
  unsigned short c = ip_checksum(ip, 20);
  ip[10] = c >> 8; ip[11] = c;
  u[1] = 67; u[3] = 68; u[4] = ulen >> 8; u[5] = ulen;
  if (radio_l2_send(r, 14 + total) != 0)
    failed = 1;
  else if (type == 3)
    leased = 1;
  return 1;
}
extern void station_wpa_poll(void);
void station_net_poll(void) {
  if (!attached)
    return;
  // Every blocking wait in this file and in sockets.c spins on this call, so the
  // supplicant's timers (group rekeys, retransmits) keep running during them.
  station_wpa_poll();
  for (unsigned i = 0; i < 8; i++) {
    int len = radio_l2_recv(frame, sizeof(frame));
    if (!len)
      break;
    if (len < 0) {
      failed = 1;
      break;
    }
    if (hosting && serve_dhcp(frame, len))
      continue;
    struct pbuf *p = pbuf_alloc(PBUF_RAW, (u16_t)len, PBUF_POOL);
    if (p && (pbuf_take(p, frame, (u16_t)len) != ERR_OK ||
              ethernet_input(p, &iface) != ERR_OK))
      pbuf_free(p);
  }
  sys_check_timeouts();
}
static void abort_connection(void) {
  if (connection) {
    tcp_recv(connection, NULL);
    tcp_err(connection, NULL);
    tcp_abort(connection);
    connection = NULL;
  }
  if (pending) {
    pbuf_free(pending);
    pending = NULL;
  }
  established = 0;
}
void station_net_close(void) {
  abort_connection();
  if (attached) {
    if (!hosting) {
      dhcp_stop(&iface);
      dhcp_cleanup(&iface);
    }
    netif_set_link_down(&iface);
    netif_set_down(&iface);
    netif_remove(&iface);
    attached = 0;
  }
  hosting = leased = 0;
}
void station_net_hostname(const unsigned char *name, unsigned n) {
  if (n >= sizeof(hostname))
    n = 0;
  memcpy(hostname, name, n);
  hostname[n] = 0;
}
void station_net_relay_password(const unsigned char *p, unsigned n) {
  if (n > sizeof(relay_password))
    n = sizeof(relay_password);
  memcpy(relay_password, p, n);
  relay_password_len = n;
}
// Host the phone's network and wait up to wait_ms for it to take the lease.
int station_net_host(const unsigned char *mac, unsigned wait_ms) {
  station_net_close();
  failed = 0;
  if (!initialized) {
    lwip_init();
    initialized = 1;
  }
  memset(&iface, 0, sizeof(iface));
  ip4_addr_t addr, mask, gw;
  IP4_ADDR(&addr, 192, 168, 77, 1);
  IP4_ADDR(&mask, 255, 255, 255, 0);
  ip4_addr_set_zero(&gw);
  if (!netif_add(&iface, &addr, &mask, &gw, NULL, setup, ethernet_input))
    return 0;
  attached = hosting = 1;
  memcpy(host_mac, mac, 6);
  memcpy(iface.hwaddr, mac, 6);
  netif_set_default(&iface);
  netif_set_up(&iface);
  netif_set_link_up(&iface);
  unsigned start = sys_now();
  while (!failed && !leased && sys_now() - start < wait_ms)
    station_net_poll();
  return !failed;
}
int station_net_leased(void) { return hosting && leased && !failed; }
// For app sockets through the relay (sockets.c): the relay's password, while the phone has a lease.
int station_relay_info(unsigned char *password, unsigned *len) {
  if (!hosting || !leased || failed)
    return 0;
  memcpy(password, relay_password, relay_password_len);
  *len = relay_password_len;
  return 1;
}
// The DHCP client of RFC 2131 §4.4 (docs/references/rfc2131.txt). Its states and
// the rules it keeps are modelled in docs/formal/dhcp; ip/test_dhcp.c holds this
// code to them.
int station_net_join(const unsigned char *mac) {
  station_net_close();
  failed = 0;
  if (!initialized) {
    lwip_init();
    initialized = 1;
  }
  memset(&iface, 0, sizeof(iface));
  if (!netif_add(&iface, NULL, NULL, NULL, NULL, setup, ethernet_input))
    return 0;
  attached = 1;
  memcpy(iface.hwaddr, mac, 6);
  netif_set_hostname(&iface, hostname[0] ? hostname : NULL);
  netif_set_default(&iface);
  netif_set_up(&iface);
  netif_set_link_up(&iface);
  unsigned start = sys_now();
  if (dhcp_start(&iface) == ERR_OK) {
    // Some networks take well over 15 s to offer an address. lwIP asks at 0, 2,
    // 6 and 14 s within this wait (four_tries in docs/formal/dhcp/DhcpClient.lean).
    while (!failed && sys_now() - start < 30000) {
      station_net_poll();
      if (!failed && dhcp_supplied_address(&iface))
        return 1;
    }
  }
  station_net_close();
  return 0;
}
static err_t connected(void *arg, struct tcp_pcb *pcb, err_t err) {
  established = err == ERR_OK;
  failed = !established;
  return ERR_OK;
}
static void error(void *arg, err_t err) {
  failed = 1;
  connection = NULL;
}
static err_t received(void *arg, struct tcp_pcb *pcb, struct pbuf *p,
                      err_t err) {
  if (!p) {
    eof = 1;
    return ERR_OK;
  }
  if (pending)
    pbuf_cat(pending, p);
  else
    pending = p;
  return ERR_OK;
}
static void resolved(const char *name, const ip_addr_t *addr, void *arg) {
  if (arg != (void *)(uintptr_t)dns_state)
    return;
  if (addr)
    destination = *addr;
  else
    failed = 1;
}
int tunnel_read(void *ctx, unsigned char *out, size_t size) {
  unsigned start = sys_now();
  while (!pending && !failed && !eof && sys_now() - request_start < 15000 &&
         sys_now() - start < 5000)
    station_net_poll();
  if (!connection || !pending || failed)
    return -1;
  unsigned n = pending->tot_len;
  if (n > size)
    n = size;
  n = pbuf_copy_partial(pending, out, n, 0);
  pending = pbuf_free_header(pending, n);
  tcp_recved(connection, n);
  return n ? (int)n : -1;
}
int tunnel_write(void *ctx, const unsigned char *buf, size_t len) {
  unsigned start = sys_now();
  while (connection && !failed && sys_now() - request_start < 15000 &&
         sys_now() - start < 5000) {
    unsigned n = tcp_sndbuf(connection);
    if (n > len)
      n = len;
    if (n && tcp_write(connection, buf, n, TCP_WRITE_FLAG_COPY) == ERR_OK) {
      tcp_output(connection);
      return n;
    }
    station_net_poll();
  }
  return -1;
}
static int read_exact(unsigned char *p, unsigned n) {
  while (n) {
    int got = tunnel_read(NULL, p, n);
    if (got <= 0)
      return 0;
    p += got;
    n -= got;
  }
  return 1;
}
static int write_all(const unsigned char *p, unsigned n) {
  while (n) {
    int put = tunnel_write(NULL, p, n);
    if (put <= 0)
      return 0;
    p += put;
    n -= put;
  }
  return 1;
}
// SOCKS5 CONNECT (RFC 1928) with username/password (RFC 1929) through the phone.
static int socks_connect(const char *host, unsigned port) {
  unsigned char b[300];
  static const unsigned char hello[] = {5, 1, 2};
  if (!write_all(hello, 3) || !read_exact(b, 2) || b[0] != 5 || b[1] != 2)
    return 0;
  unsigned n = 0;
  b[n++] = 1;
  b[n++] = 6;
  memcpy(b + n, "violet", 6);
  n += 6;
  b[n++] = relay_password_len;
  memcpy(b + n, relay_password, relay_password_len);
  n += relay_password_len;
  if (!write_all(b, n) || !read_exact(b, 2) || b[1] != 0)
    return 0;
  unsigned hl = strlen(host);
  n = 0;
  b[n++] = 5; b[n++] = 1; b[n++] = 0; b[n++] = 3; b[n++] = hl;
  memcpy(b + n, host, hl);
  n += hl;
  b[n++] = port >> 8;
  b[n++] = port;
  if (!write_all(b, n) || !read_exact(b, 4) || b[0] != 5 || b[1] != 0)
    return 0;
  unsigned rest = b[3] == 1 ? 6 : b[3] == 4 ? 18 : 0;
  if (b[3] == 3) {
    if (!read_exact(b, 1))
      return 0;
    rest = b[0] + 2;
  }
  return rest && read_exact(b, rest);
}
// Open a TCP stream ready for TLS to host:443. When hosting the phone's network,
// the stream goes through the phone's SOCKS5 relay and the phone resolves `host`.
static int relay_open(const char *host, unsigned port, int attempts) {
  IP4_ADDR(ip_2_ip4(&destination), 192, 168, 77, 2);
  // The companion may still be starting its listener: retry (2 s per attempt).
  for (int attempt = 0; attempt < attempts && !established; attempt++) {
    abort_connection();
    failed = 0;
    connection = tcp_new();
    if (!connection)
      return 0;
    tcp_err(connection, error);
    tcp_recv(connection, received);
    unsigned start = sys_now();
    if (tcp_connect(connection, &destination, 1081, connected) == ERR_OK)
      while (!established && !failed && sys_now() - start < 2000)
        station_net_poll();
  }
  request_start = sys_now();
  return established && !failed && socks_connect(host, port);
}
static int station_open(const char *host) {
  static unsigned generation;
  dns_state = ++generation;
  ip_addr_set_zero(&destination);
  err_t rc = dns_gethostbyname(host, &destination, resolved,
                               (void *)(uintptr_t)dns_state);
  if (rc != ERR_OK && rc != ERR_INPROGRESS)
    return 0;
  unsigned start = sys_now();
  while (ip_addr_isany_val(destination) && !failed && sys_now() - start < 5000)
    station_net_poll();
  if (failed || ip_addr_isany_val(destination))
    return 0;
  connection = tcp_new();
  if (!connection)
    return 0;
  tcp_err(connection, error);
  tcp_recv(connection, received);
  if (tcp_connect(connection, &destination, 443, connected) == ERR_OK) {
    start = sys_now();
    while (!established && !failed && sys_now() - start < 8000)
      station_net_poll();
  }
  return established && !failed;
}
static int open_https(const char *host) {
  if (hosting ? !(attached && leased) : !(attached && dhcp_supplied_address(&iface)))
    return 0;
  abort_connection();
  failed = 0;
  eof = 0;
  request_start = sys_now();
  return hosting ? relay_open(host, 443, 5) : station_open(host);
}
// Violet Relay answers a CONNECT to "time.violet" with the phone's Unix time as four
// big-endian bytes. One quick attempt: Radio's event loop retries every few seconds.
int station_net_phone_time(unsigned *out) {
  if (!hosting || !attached || !leased)
    return 0;
  abort_connection();
  failed = 0;
  eof = 0;
  request_start = sys_now();
  unsigned char b[4];
  int ok = relay_open("time.violet", 1, 1) && read_exact(b, 4);
  abort_connection();
  if (ok)
    *out = (unsigned)b[0] << 24 | (unsigned)b[1] << 16 | (unsigned)b[2] << 8 | b[3];
  return ok;
}
int station_net_uuid(unsigned char *out) {
  int ok = open_https("www.uuidgenerator.net") && guest_https(out);
  abort_connection();
  return ok;
}
