#pragma once
#define NO_SYS 1
#define MEM_ALIGNMENT 4
#define MEM_SIZE 24576
#define LWIP_SOCKET 0
#define LWIP_NETCONN 0
#define LWIP_STATS 0
#define LWIP_DHCP 1
#define LWIP_DNS 1
#define LWIP_DNS_SECURE 7
#define LWIP_IPV6 0
#define LWIP_IPV4 1
#define LWIP_IGMP 0
#define IP_FORWARD 0
#define IP_REASSEMBLY 0
#define IP_FRAG 0
#define LWIP_RAW 0
#define TCP_MSS 1460
#define TCP_WND (4*TCP_MSS)
#define TCP_SND_BUF (4*TCP_MSS)
#define TCP_SND_QUEUELEN 16
// Open apps' sockets (sockets.c) plus Radio's own HTTPS connection.
#define MEMP_NUM_TCP_PCB 10
// DHCP, DNS and open apps' UDP sockets.
#define MEMP_NUM_UDP_PCB 8
#define PBUF_POOL_SIZE 16
#define LWIP_NETIF_HOSTNAME 1
#define LWIP_NETIF_STATUS_CALLBACK 0
#define LWIP_TIMEVAL_PRIVATE 0
#define SYS_LIGHTWEIGHT_PROT 0

#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS 1
