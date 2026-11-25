// The Moto DHCP client against a scripted network on a virtual clock, for the
// cases a real network produces only by accident: no answer, a late offer, a
// refusal, an access point answering ARP for the phone, a dead link, a lease
// that cannot be renewed.
//
// The client is RFC 2131's (docs/references/rfc2131.txt, §4.4 and Figure 5).
// Every time the link is polled, observe() holds the code to the states, steps
// and invariants of its models, docs/formal/dhcp/DhcpClient.tla and
// DhcpClient.lean. Two checks at the end are for later RFCs: an option the
// server splits (RFC 3396) and what the client says about itself (RFC 7844).
#include "lwip/dhcp.h"
#include "lwip/netif.h"
#include "lwip/prot/dhcp.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define SERVER 0x0a000202u /* 10.0.2.2 */
#define POOL 0x0a00020fu   /* 10.0.2.15, the first address the server offers */
#define JOIN_MS 30000u     /* how long station_net_join waits for an address */
#define FOREVER (1 << 30)
int station_net_join(const unsigned char *);
void station_net_close(void), station_net_poll(void);
static const unsigned char client_mac[] = {2, 0, 0, 0, 0, 1}, server_mac[] = {2, 0, 0, 0, 0, 2};
// What the network does in this case. Times are milliseconds from the start of the case.
static struct {
  unsigned deaf_ms;     // the server misses everything the client sends before this
  unsigned offer_delay; // how long the server takes to offer an address
  int naks;             // requests the server refuses before it accepts one
  int proxy_arp;        // the access point answers ARP for the address leased to the phone
  unsigned lease;       // seconds
  unsigned link_dies;   // the link fails at this time; 0 is never
  int silent;           // nothing on the network answers
  int split;            // the server sends the lease time as two options (RFC 3396)
} net;
// What the client sent.
static struct {
  unsigned frames, discovers, requests, renewals, declines;
  unsigned acks; // received, not sent
} seen;
// For each option code, the message types that carried it (bit n: type n), over every case.
static unsigned sent[256];
static const char *scenario;
static unsigned now, started, seed, acked_at;
static struct {
  unsigned due;
  size_t len;
  unsigned char data[14 + 20 + 8 + 300];
} queue[16];
static unsigned queued;
#define CHECK(ok, what)                                                        \
  do {                                                                         \
    if (!(ok)) {                                                               \
      fprintf(stderr, "%s: %s (test_dhcp.c:%d)\n", scenario, what, __LINE__);  \
      exit(1);                                                                 \
    }                                                                          \
  } while (0)

unsigned radio_monotonic_ms(void) { return now; }
// The same numbers every run, so the client's random waits do not move the results.
int radio_random(unsigned char *out, size_t n) {
  while (n--) {
    seed = seed * 1103515245 + 12345;
    *out++ = seed >> 16;
  }
  return 0;
}
// The IP client polls the supplicant's timers; there is no supplicant here.
void station_wpa_poll(void) {}
// No TLS is linked: nothing here opens a connection.
int guest_https(unsigned char *out) { return 0; }

static void put32(unsigned char *p, uint32_t v) {
  p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v;
}
static uint32_t get32(const unsigned char *p) {
  return (uint32_t)p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3];
}
static void deliver(const unsigned char *frame, size_t n, unsigned delay) {
  if (queued == 16) {
    fprintf(stderr, "%s: the test network's queue is full\n", scenario);
    exit(1);
  }
  queue[queued].due = now + delay;
  queue[queued].len = n;
  memcpy(queue[queued++].data, frame, n);
}
static int answering(void) {
  return !net.silent && now - started >= net.deaf_ms;
}
// One DHCP message from the server: 2 offer, 5 acknowledge, 6 refuse.
static void reply(const unsigned char *bootp, int type, uint32_t addr, unsigned delay) {
  unsigned char r[14 + 20 + 8 + 300] = {0}, *ip = r + 14, *u = ip + 20, *b = u + 8, *o = b + 240;
  memset(r, 0xff, 6);
  memcpy(r + 6, server_mac, 6);
  r[12] = 8;
  ip[0] = 0x45; ip[2] = 328 >> 8; ip[3] = 328 & 255; ip[8] = 64; ip[9] = 17;
  put32(ip + 12, SERVER);
  put32(ip + 16, 0xffffffffu);
  unsigned sum = 0;
  for (unsigned i = 0; i < 20; i += 2)
    sum += ip[i] << 8 | ip[i + 1];
  sum = ~((sum & 0xffff) + (sum >> 16));
  ip[10] = sum >> 8; ip[11] = sum;
  u[1] = 67; u[3] = 68; u[4] = 308 >> 8; u[5] = 308 & 255;
  b[0] = 2; b[1] = 1; b[2] = 6;
  memcpy(b + 4, bootp + 4, 4); /* xid */
  put32(b + 16, addr);
  put32(b + 20, SERVER);
  memcpy(b + 28, bootp + 28, 16);
  b[236] = 99; b[237] = 130; b[238] = 83; b[239] = 99;
  *o++ = 53; *o++ = 1; *o++ = type;
  *o++ = 54; *o++ = 4; put32(o, SERVER); o += 4;
  if (type != 6) {
    *o++ = 51; *o++ = net.split ? 2 : 4; put32(o, net.lease); o += 2;
    if (net.split) {
      *o++ = 51; *o++ = 2;
    }
    o += 2;
    *o++ = 1; *o++ = 4; put32(o, 0xffffff00u); o += 4;
    *o++ = 3; *o++ = 4; put32(o, SERVER); o += 4;
  }
  *o++ = 255;
  deliver(r, sizeof(r), delay);
}
static void dhcp(const unsigned char *b, size_t n) {
  int type = 0;
  uint32_t requested = 0, held = get32(b + 12);
  unsigned char codes[32];
  unsigned count = 0;
  for (size_t o = 240; o + 2 < n && b[o] != 255;) {
    if (!b[o]) {
      o++;
      continue;
    }
    if (b[o] == 53)
      type = b[o + 2];
    if (b[o] == 50 && o + 6 <= n)
      requested = get32(b + o + 2);
    if (count < sizeof(codes))
      codes[count++] = b[o];
    o += 2 + b[o + 1];
  }
  while (count)
    sent[codes[--count]] |= 1u << type;
  if (type == 1) {
    seen.discovers++;
    if (answering())
      reply(b, 2, POOL, net.offer_delay);
  } else if (type == 3) {
    // A renewal names the address the client holds; a first request names the one offered.
    if (held)
      seen.renewals++;
    else
      seen.requests++;
    if (!answering())
      return;
    if (!held && net.naks) {
      net.naks--;
      reply(b, 6, 0, 0);
    } else {
      reply(b, 5, held ? held : requested, 0);
    }
  } else if (type == 4) {
    seen.declines++;
  }
}
// The server answers for its own address. An access point that proxies ARP
// answers for the phone's address too, with the phone's own MAC.
static void arp(const unsigned char *f) {
  if (f[20] != 0 || f[21] != 1 || net.silent)
    return;
  uint32_t from = get32(f + 28), wanted = get32(f + 38);
  const unsigned char *mac = wanted == SERVER ? server_mac
                             : wanted == POOL && net.proxy_arp ? client_mac : NULL;
  if (!mac)
    return;
  unsigned char r[42] = {0};
  memcpy(r, f + 6, 6);
  memcpy(r + 6, mac, 6);
  r[12] = 8; r[13] = 6; r[15] = 1; r[16] = 8; r[18] = 6; r[19] = 4; r[21] = 2;
  memcpy(r + 22, mac, 6);
  put32(r + 28, wanted);
  memcpy(r + 32, f + 22, 6);
  put32(r + 38, from);
  deliver(r, sizeof(r), 0);
}
int radio_l2_send(const unsigned char *f, size_t n) {
  seen.frames++;
  if (n >= 42 && f[12] == 8 && f[13] == 6)
    arp(f);
  else if (n >= 42 + 240 && f[12] == 8 && f[13] == 0 && f[14] == 0x45 && f[23] == 17 &&
           f[36] == 0 && f[37] == 67)
    dhcp(f + 42, n - 42);
  return 0;
}
// The client's states, as the models name them.
enum { OFF, SELECTING, REQUESTING, CHECKING, BOUND, RENEWING, REBINDING, PHASES };
static const char *const phase_name[] = {"OFF",   "SELECTING", "REQUESTING", "CHECKING",
                                         "BOUND", "RENEWING",  "REBINDING"};
// The steps the models allow. station_net_close also takes any state to OFF.
static const char allowed[PHASES][PHASES] = {
    [OFF][SELECTING] = 1,        // station_net_join
    [SELECTING][REQUESTING] = 1, // RecvOffer
    [REQUESTING][BOUND] = 1,     // RecvAck
    [REQUESTING][CHECKING] = 1,  // RecvAck with the address check on
    [REQUESTING][SELECTING] = 1, // RecvNak, RequestTimeout
    [CHECKING][BOUND] = 1,       // CheckClear
    [CHECKING][SELECTING] = 1,   // CheckConflict
    [BOUND][RENEWING] = 1,       // Tick at T1
    [RENEWING][BOUND] = 1,       // RecvAck
    [RENEWING][REBINDING] = 1,   // Tick at T2
    [RENEWING][SELECTING] = 1,   // RecvNak
    [REBINDING][BOUND] = 1,      // RecvAck
    [REBINDING][SELECTING] = 1,  // RecvNak, Tick at the lease's end
};
static int phase(void) {
  struct dhcp *dhcp = netif_default ? netif_dhcp_data(netif_default) : NULL;
  switch (dhcp ? dhcp->state : DHCP_STATE_OFF) {
  case DHCP_STATE_OFF: return OFF;
  // lwIP passes through these two on its way to sending DHCPDISCOVER.
  case DHCP_STATE_INIT:
  case DHCP_STATE_BACKING_OFF:
  case DHCP_STATE_SELECTING: return SELECTING;
  case DHCP_STATE_REQUESTING: return REQUESTING;
  case DHCP_STATE_CHECKING: return CHECKING;
  case DHCP_STATE_BOUND: return BOUND;
  case DHCP_STATE_RENEWING: return RENEWING;
  case DHCP_STATE_REBINDING: return REBINDING;
  default: return PHASES;
  }
}
// Hold the code to the models: only their steps, and their invariants in every
// state. lwIP counts a lease in whole minutes, so it may renew a minute early.
static void observe(void) {
  static int last = OFF;
  int p = phase();
  CHECK(p != PHASES, "entered a state the models do not have");
  if (p != last && p != OFF && !allowed[last][p]) {
    fprintf(stderr, "%s: went from %s to %s, a step the models do not have\n", scenario,
            phase_name[last], phase_name[p]);
    exit(1);
  }
  last = p;
  int uses = p == BOUND || p == RENEWING || p == REBINDING;
  unsigned held = now - acked_at, lease_ms = net.lease * 1000;
  CHECK(uses == (netif_default && get32((const unsigned char *)netif_ip4_addr(netif_default))),
        "UsesAddress: the interface's address does not match the client's state");
  CHECK(!uses || seen.acks, "AddressWasGranted: used an address no server acknowledged");
  CHECK(!uses || held <= lease_ms, "LeaseLive: used an address after its lease ran out");
  CHECK(p != RENEWING || held + 60000 >= lease_ms / 2, "TimersRespected: renewed before T1");
  CHECK(p != REBINDING || held + 60000 >= lease_ms / 8 * 7, "TimersRespected: rebound before T2");
}
// Each look at the link takes a millisecond, so waits end without real time passing.
int radio_l2_recv(unsigned char *out, size_t capacity) {
  observe();
  now++;
  if (net.link_dies && now - started >= net.link_dies)
    return -1;
  for (unsigned i = 0; i < queued; i++) {
    if ((int)(now - queue[i].due) < 0)
      continue;
    size_t n = queue[i].len;
    if (n > capacity)
      return -1;
    memcpy(out, queue[i].data, n);
    memmove(&queue[i], &queue[i + 1], (--queued - i) * sizeof(queue[0]));
    // lwIP counts a lease from the DHCPACK's arrival.
    if (n == sizeof(queue[0].data) && out[42 + 242] == 5) {
      seen.acks++;
      acked_at = now;
    }
    return n;
  }
  return 0;
}

static void begin(const char *name) {
  station_net_close();
  observe();
  scenario = name;
  memset(&net, 0, sizeof(net));
  memset(&seen, 0, sizeof(seen));
  net.lease = 3600;
  queued = 0;
  seed = 1;
  started = now;
}
// The address DHCP gave the interface; 0 without one.
static uint32_t address(void) {
  if (!netif_default || !dhcp_supplied_address(netif_default))
    return 0;
  return get32((const unsigned char *)netif_ip4_addr(netif_default));
}
static void run(unsigned ms) {
  for (unsigned start = now; now - start < ms;) {
    unsigned before = now;
    station_net_poll();
    observe();
    // With the interface down nothing looks at the link, so time passes here.
    if (now == before)
      now++;
  }
}
static int join(void) {
  int ok = station_net_join(client_mac);
  observe();
  return ok;
}
// A failed join must take the interface down and stop sending (FailedIsDown, Quiet).
static void check_gave_up(void) {
  CHECK(!netif_default, "left the interface up after a failed join");
  unsigned frames = seen.frames;
  run(5000);
  CHECK(seen.frames == frames, "kept sending after a failed join");
}
// A phone with a random hardware address should tell the network nothing else
// about itself (RFC 7844 §3, docs/references/rfc7844.txt). RFC 4361 §6.1 asks
// for the opposite, an identifier that stays the same; lwIP sends none.
static void check_sent(void) {
  scenario = "what the client sent";
  static const struct {
    int code;
    const char *what;
  } naming[] = {{12, "a host name"},     {43, "vendor information"}, {60, "a vendor class"},
                {61, "a client identifier"}, {77, "a user class"},       {81, "a domain name"},
                {124, "a vendor class"}, {125, "vendor information"}};
  for (unsigned i = 0; i < sizeof(naming) / sizeof(naming[0]); i++) {
    if (sent[naming[i].code]) {
      fprintf(stderr, "%s: %s (option %d)\n", scenario, naming[i].what, naming[i].code);
      exit(1);
    }
  }
  CHECK(!(sent[50] & 1 << 1), "DHCPDISCOVER asked for an address the phone had before");
  printf("%s: options", scenario);
  for (unsigned code = 0; code < 256; code++)
    if (sent[code])
      printf(" %u", code);
  puts(", none of them naming the phone");
}
static unsigned seconds(unsigned ms) { return (ms + 500) / 1000; }

int main(void) {
  begin("no answer");
  net.silent = 1;
  CHECK(!join(), "joined a network that never answered");
  unsigned waited = now - started;
  CHECK(waited >= JOIN_MS && waited < JOIN_MS + 100, "did not give up after 30 s");
  CHECK(seen.discovers >= 4, "asked fewer than four times");
  check_gave_up();
  printf("no answer: gave up after %u s and %u requests for an address\n", seconds(waited), seen.discovers);
  net.silent = 0;
  started = now;
  CHECK(join() && address() == POOL, "could not join once the network answered");
  CHECK(now - started < 1000, "waited before using the address the server granted");
  printf("then an answer: address in use after %u ms\n", now - started);

  begin("first requests lost");
  net.deaf_ms = 10000;
  CHECK(join() && address() == POOL, "did not join after 10 s of lost requests");
  printf("first requests lost for 10 s: address in use after %u s\n", seconds(now - started));

  begin("slow offer");
  net.offer_delay = 28000;
  CHECK(join() && address() == POOL, "did not wait 28 s for an offer");
  printf("offer after 28 s: address in use after %u s\n", seconds(now - started));

  begin("one refusal");
  net.naks = 1;
  CHECK(join() && address() == POOL, "did not start again after a refusal");
  CHECK(seen.discovers >= 2 && seen.requests >= 2, "did not ask again after a refusal");
  printf("one refusal: asked again, address in use after %u ms\n", now - started);

  begin("every request refused");
  net.naks = FOREVER;
  CHECK(!join(), "joined although every request was refused");
  waited = now - started;
  CHECK(waited < JOIN_MS + 100, "did not give up after 30 s");
  check_gave_up();
  printf("every request refused: gave up after %u s and %u requests\n", seconds(waited), seen.requests);

  begin("access point answers ARP for the phone");
  net.proxy_arp = 1;
  CHECK(join() && address() == POOL, "did not join through a proxy-ARP access point");
  run(10000);
  CHECK(address() == POOL && !seen.declines, "gave back its own address");
  printf("access point answers ARP for the phone: address in use after %u ms and kept\n", now - started - 10000);

  begin("link fails");
  net.silent = 1;
  net.link_dies = 3000;
  CHECK(!join(), "joined over a dead link");
  CHECK(now - started < 3100, "kept waiting on a dead link");
  net.link_dies = 0;
  check_gave_up();
  printf("link fails after 3 s: gave up at once\n");

  begin("lease renewed");
  net.lease = 600;
  CHECK(join(), "did not join");
  run(900000);
  CHECK(seen.renewals, "never asked to renew");
  CHECK(address() == POOL, "lost a lease the server renewed");
  printf("10-minute lease, server answers: still held after 15 minutes, %u renewals\n", seen.renewals);

  begin("lease runs out");
  net.lease = 600;
  CHECK(join(), "did not join");
  net.silent = 1;
  unsigned discovers = seen.discovers;
  run(900000);
  CHECK(!address(), "kept an address after its lease ran out");
  CHECK(seen.discovers > discovers, "did not look for a new lease");
  printf("10-minute lease, server gone: address dropped, asking again\n");

  // RFC 3396 has a client join the pieces of an option sent in several parts. lwIP
  // does not: it takes the offer as malformed and drops it.
  begin("server splits an option");
  net.split = 1;
  CHECK(!join(), "joined on an offer whose lease time came in two parts");
  waited = now - started;
  CHECK(seen.discovers >= 4 && !seen.requests, "answered an offer it should have dropped");
  check_gave_up();
  printf("server splits an option in two: %u offers dropped, gave up after %u s\n", seen.discovers, seconds(waited));

  station_net_close();
  observe();
  check_sent();
  puts("Moto DHCP client passed 11 cases against a scripted network, within its models at every step");
  return 0;
}
