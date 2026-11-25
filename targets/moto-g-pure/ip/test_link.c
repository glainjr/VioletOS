// Stand-in Ethernet link for the native Moto IP client. No TLS or HTTP runs
// here.
#include <arpa/inet.h>
#include <libslirp.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
static Slirp *net;
static struct pollfd fds[64];
static unsigned count, head, tail;
static struct {
  size_t len;
  unsigned char data[1518];
} queue[32];
static int64_t clock_ns(void *unused) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (int64_t)t.tv_sec * 1000000000 + t.tv_nsec;
}
unsigned radio_monotonic_ms(void) { return clock_ns(NULL) / 1000000; }
unsigned radio_unix_time(void) { return time(NULL); }
int radio_random(unsigned char *out, size_t n) {
  arc4random_buf(out, n);
  return 0;
}
// The IP client polls the supplicant's timers; there is no supplicant here.
void station_wpa_poll(void) {}
static void socket_changed(slirp_os_socket fd, void *opaque) {}
static void noop(void *unused) {}
static void error(const char *msg, void *unused) {
  fprintf(stderr, "%s\n", msg);
}
static slirp_ssize_t receive(const void *data, size_t n, void *unused) {
  if (tail - head >= 32 || n > sizeof(queue[0].data))
    return -1;
  unsigned i = tail++ % 32;
  queue[i].len = n;
  memcpy(queue[i].data, data, n);
  return n;
}
// The host name option (12) of the last DHCP message the client sent; -1 without one.
static char sent_name[256];
static int sent_name_len = -1, dhcp_messages;
static void inspect_dhcp(const unsigned char *f, size_t n) {
  if (n < 14 + 20 + 8 + 240 || f[12] != 8 || f[13] != 0 || f[23] != 17)
    return;
  size_t udp = 14 + (f[14] & 15) * 4;
  if (udp + 8 + 240 > n || f[udp + 2] != 0 || f[udp + 3] != 67)
    return;
  dhcp_messages++;
  sent_name_len = -1;
  // Options follow BOOTP's 236 fixed bytes and the 4-byte magic cookie.
  for (size_t o = udp + 8 + 240; o < n && f[o] != 255;) {
    if (!f[o]) {
      o++;
      continue;
    }
    if (o + 2 > n || o + 2 + f[o + 1] > n)
      return;
    if (f[o] == 12) {
      sent_name_len = f[o + 1];
      memcpy(sent_name, f + o + 2, f[o + 1]);
    }
    o += 2 + f[o + 1];
  }
}
int radio_l2_send(const unsigned char *data, size_t n) {
  inspect_dhcp(data, n);
  slirp_input(net, data, n);
  return 0;
}
static int add(slirp_os_socket fd, int events, void *unused) {
  if (count >= 64)
    abort();
  short e = 0;
  if (events & SLIRP_POLL_IN)
    e |= POLLIN;
  if (events & SLIRP_POLL_OUT)
    e |= POLLOUT;
  if (events & SLIRP_POLL_PRI)
    e |= POLLPRI;
  fds[count] = (struct pollfd){fd, e, 0};
  return count++;
}
static int ready(int i, void *unused) {
  short e = fds[i].revents;
  return ((e & POLLIN) ? SLIRP_POLL_IN : 0) |
         ((e & POLLOUT) ? SLIRP_POLL_OUT : 0) |
         ((e & POLLERR) ? SLIRP_POLL_ERR : 0) |
         ((e & POLLHUP) ? SLIRP_POLL_HUP : 0) |
         ((e & POLLPRI) ? SLIRP_POLL_PRI : 0);
}
int radio_l2_recv(unsigned char *out, size_t capacity) {
  if (head == tail) {
    uint32_t wait = 1;
    count = 0;
    slirp_pollfds_fill_socket(net, &wait, add, NULL);
    int n = poll(fds, count, wait > 1 ? 1 : (int)wait);
    slirp_pollfds_poll(net, n < 0, ready, NULL);
  }
  if (head == tail)
    return 0;
  unsigned i = head++ % 32;
  if (queue[i].len > capacity)
    return -1;
  memcpy(out, queue[i].data, queue[i].len);
  return queue[i].len;
}
int station_net_join(const unsigned char *), station_net_uuid(unsigned char *);
void station_net_close(void), station_net_hostname(const unsigned char *, unsigned);
int main(void) {
  SlirpConfig conf = {
      .version = 6, .in_enabled = true, .disable_host_loopback = true};
  inet_pton(AF_INET, "10.0.2.0", &conf.vnetwork);
  inet_pton(AF_INET, "255.255.255.0", &conf.vnetmask);
  inet_pton(AF_INET, "10.0.2.2", &conf.vhost);
  inet_pton(AF_INET, "10.0.2.15", &conf.vdhcp_start);
  inet_pton(AF_INET, "10.0.2.3", &conf.vnameserver);
  SlirpCb callbacks = {.register_poll_socket = socket_changed,
                       .unregister_poll_socket = socket_changed,
                       .send_packet = receive,
                       .clock_get_ns = clock_ns,
                       .notify = noop,
                       .guest_error = error};
  net = slirp_new(&conf, &callbacks, NULL);
  if (!net)
    return 1;
  const unsigned char mac[] = {2, 0, 0, 0, 0, 1};
  unsigned char id[37] = {0};
  if (!station_net_join(mac)) {
    fputs("DHCP failed\n", stderr);
    return 2;
  }
  if (!dhcp_messages || sent_name_len != -1) {
    fputs("DHCP sent a host name without one being set\n", stderr);
    return 4;
  }
  static const char name[] = "violet-test";
  station_net_hostname((const unsigned char *)name, sizeof name - 1);
  if (!station_net_join(mac)) {
    fputs("DHCP failed with a host name\n", stderr);
    return 2;
  }
  if (sent_name_len != sizeof name - 1 || memcmp(sent_name, name, sizeof name - 1)) {
    fputs("DHCP did not carry the host name\n", stderr);
    return 5;
  }
  puts("Moto IP client acquired DHCP lease on test Ethernet, naming itself only when a host name is set");
  fflush(stdout);
  int ok = station_net_uuid(id);
  station_net_close();
  slirp_cleanup(net);
  if (!ok) {
    fputs("UUID request failed\n", stderr);
    return 3;
  }
  printf("Verified HTTPS UUID %s\n", id);
  return 0;
}
