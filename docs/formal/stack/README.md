# From radio power to an open TCP connection, modelled

What the Moto G Pure does between "Wi-Fi off" and the first TCP connection of an app such as Weather, step by step: the code that does it, the standard that governs it, how long it waits, and the number it reports when it fails. Two machine-checked models state the rules the steps must keep.

| File | What it is | Checked by |
|---|---|---|
| `WifiStack.lean` | The layers as a function, with proofs that hold for every sequence of events; the arithmetic of the clock, the scan, the retries and the station address. | `lean` |
| `WifiStack.tla` and the `.cfg` files | The same path with access points, a lossy air and retries, on ten small networks. | TLC |

`make check-stack-model` runs both, as `make check-dhcp-model` does for [the DHCP client](../dhcp/README.md); the tools are the same. Neither runs in CI.

**Nothing yet holds the radio code to these models.** The DHCP model has `ip/test_dhcp.c`, which runs the real client and checks every step against it. This one was written by reading the code. Until it has such a test, a rule proved here is a rule about the model.

Time is not in the TLA+ model: a wait there runs out only when nothing more can arrive. The Lean file has the arithmetic of the real limits.

## The path

Code is under `targets/moto-g-pure` unless a path says otherwise. "Fails as" is the step number `build/moto-package join-wifi` prints.

| # | Step | Code | Governed by | Tries and waits | Fails as |
|---|---|---|---|---|---|
| 1 | Power: the supervisor powers the connectivity domain and Radio starts the chip's ROM | `radio.rs` `start_connectivity`, `boot_rom` | MediaTek's driver; no public standard | 10 tries, 240 ms each | 2–6, 8–16 |
| 2 | Firmware: the WMT patch, then the WLAN firmware | `wmt.rs`, `wlan_download.rs`, `wlan_axi.rs` `boot` | the same | 1.5–1.8 s a step | 20–39, 42 |
| 3 | Station address: a fixed one if provisioned, else the chip's, else a random one | `wlan_axi.rs` `station_address`, `radio_io.rs` `fixed_address` | RFC 9542 §2.1; RFC 7844 and RFC 9724 on why it is random | | 45 |
| 4 | Scan: passive, 20 channels | `passive_scan.rs`, `wlan_axi.rs` `scan` | IEEE 802.11 scanning | one scan, 4.5 s | 41, or 49 if the network is not found |
| 5 | Authentication: Open System, to each access point found in turn | `station.rs` `request`, `wlan_axi.rs` `management_exchange` | IEEE 802.11 authentication | 1 frame, 700 ms | 47; 49 if refused |
| 6 | Association | the same, then `commit_peer` | IEEE 802.11 association | 1 frame, 700 ms | 48; 49 if refused |
| 7 | WPA2 4-way handshake | `wpa.rs` `authorize`, `supplicant.c` over hostap 2.11's `rsn_supp` | IEEE 802.11 RSNA key management; NIST SP 800-97 §5.5.1. Key from the password by PBKDF2 (RFC 8018 §5.2), integrity by HMAC-SHA1 (RFC 2104), group key by AES Key Wrap (RFC 3394), data by AES-CCM (RFC 3610) | 8 s; the access point repeats its messages | 52 after three messages without keys, else 57; 50 with no key |
| 8 | DHCP | `ip/client.c` `station_net_join` over lwIP 2.2.1 | RFC 2131 and the rest in [the DHCP model](../dhcp/README.md). Frames carry IP behind an LLC/SNAP header (RFC 1042), which `station.rs` `ethernet` strips | 4 requests in 30 s | 53 |
| 9 | ARP for the router | lwIP `etharp` | RFC 826; RFC 1122 §2.3.2 | one a second | inside 10 and 11 |
| 10 | DNS for the server's name | `ip/sockets.c` `direct_connect` over lwIP `dns` | RFC 1035; RFC 1123 §6.1.3.3; RFC 5452 | 4 queries in 5 s | the app's "no such name" |
| 11 | TCP handshake | `ip/sockets.c` `tcp_open` over lwIP `tcp` | RFC 9293 §3.5; RFC 6298 | 3 SYNs in 8 s | the app's timeout |

A join that fails shuts the chip down and cuts its power. A link lost once the phone is online is rejoined with the firmware still running (`radio.rs` `relink`).

## What is established

| Rule | Source | Lean | TLA+ |
|---|---|---|---|
| Association only after authentication, keys only on an association | IEEE 802.11 states 1–4 | `sound_run` | `StatesInOrder` |
| No IP frame before the keys: DHCP, ARP, DNS and TCP all wait for them | IEEE 802.1X controlled port; NIST SP 800-97 §5.5.1 | `no_ip_before_keys` | `NoIpBeforeKeys` |
| An open TCP connection rests on every layer below it | | `open_needs_every_layer` | `Layered` |
| Without the right key there are never keys, an address or a connection | | `wrong_key_never_online` | `KeyNeeded`, `WrongKey.cfg` |
| A wrong key is reported as 52 | `wpa.rs` | | `ReportsWrongKey` |
| A failed join leaves the chip without power and nothing in use | `radio.rs` | `failed_is_off` | `FailedIsOff` |
| Each failure number comes from its own step | `radio.rs` | | `CodeFits` |
| Only an access point that the scan found and that is in reach is joined | | | `StatesInOrder` |
| With every access point in reach and nothing lost, a join always ends in an open connection | | | `Connects` (`Ideal.cfg`) |
| An access point that is found but out of reach costs one wait and does not stop the join | `radio.rs` | | `Connects` (`FarAp.cfg`) |
| A second access point in reach covers one lost exchange | | | `Connects` (`OneLossTwoAps.cfg`) |
| One lost exchange can end a join only at authentication or association | | | `OneLossOnlyHurtsTheStart` (`OneLoss.cfg`) |
| The rules above hold with faults, missed beacons, losses and a deauthentication together | | | `Rough.cfg` |
| A wait checked before Radio's clock comes round gives the right answer | `radio_io.rs` | `expired_exact` | |
| The longest wait Radio asks for, 150 s, fits the clock | `radio.rs` | `longest_wait_fits` | |
| The random station address is always unicast and locally assigned | RFC 9542 §2.1 | `random_address_is_local` | |
| What the LLC/SNAP header wraps comes back unwrapped | RFC 1042 | `snap_round_trip` | |

## Where a join fails though the network is there

Four of the networks are ones where TLC must find a rule broken. Each is a way the phone, as built, fails to join a network that would take it.

| Network | Rule broken | What it shows |
|---|---|---|
| `OneLossFails.cfg` | `ReachableMeansJoined` | One access point in reach and one lost exchange. Authentication and association are sent once each, so the join ends with 47 or 48. |
| `MissedBeacon.cfg` | `ReachableMeansJoined` | The scan stays on a channel for one beacon's time (`one_beacon_per_stay`). A beacon lost in the air is an access point not found, and the join ends with 49. |
| `Unseen.cfg` | `ReachableMeansJoined` | The only access point in reach is on a channel the scan does not listen on (`never_scanned`: 12–14 and 52–144). The join tries the one it found and ends with 47. |
| `Deauth.cfg` | `NoAddressMeansWaited` | A deauthentication during DHCP ends the join with 53 at once. "No address" then does not mean the DHCP server was silent for 30 s. |

## Where the phone departs from the standards and from common practice

DHCP's departures are in [the DHCP model](../dhcp/README.md). These are the rest. "Practice" is what Linux 6.6 does (`net/mac80211/mlme.c`, `scan.c`); it is a comparison, not a requirement.

### Radio (this project's code)

| Departure | Against | Effect |
|---|---|---|
| Authentication and association are each sent once, with a 700 ms wait | Practice: three tries, 200 ms apart. The standard leaves the count to the implementation | One exchange that fails ends the attempt on that access point: `OneLossFails.cfg`. |
| The scan is passive and stays 120 ms on each channel (the 120 in the scan request, taken to be milliseconds) | Practice: probe requests on channels that allow them, and 111 ms on passive ones | Fewer access points found, and none with a hidden name: `MissedBeacon.cfg`. |
| Channels 12–14 and 52–144 are not scanned, and cannot be joined (`channel_for` refuses them) | The channels a regulatory domain allows | An access point there does not exist for the phone: `Unseen.cfg`. |
| Access points are tried in the order found, 2.4 GHz first, with no measure of signal | Practice: strongest first | Up to 1.7 s is spent on each access point that is found and does not answer, and 3.1 s at worst on one that answers late. `join_fits_host_wait`: with 14 or more of one name at their worst, the join outlasts the 85 s `moto-package` waits. |
| No protected management frames (the RSN capabilities sent are zero) | IEEE 802.11 management frame protection, optional in WPA2 and required in WPA3 | Any deauthentication naming the access point is believed. A network that requires protection, WPA3-only included, is not offered. |
| A deauthentication during the handshake is not looked for | | The join waits out the 8 s and reports 57, or 52 if three handshake messages had arrived. |
| `VIOLET_FIXED_MAC` may be any unicast address | RFC 9542 §2.1: an address a network operator picks has the local bit set | `fixed_address_need_not_be_local`: an address such as `04:…` is accepted though it lies in the space the IEEE assigns to manufacturers. Use one whose first byte is `02`, `06`, `0a` or `0e`. |
| Waits are timed on a 32-bit count of a 13 MHz clock, which comes round every 330 s | | `expired_exact` and `longest_wait_fits`: every wait Radio makes is right, if it is checked before the count comes round. `missed_after_a_turn`: one checked later is not. |

### lwIP 2.2.1 as shipped

| Departure | Against | Effect |
|---|---|---|
| A DNS query is repeated after 1, 1 and 2 s | RFC 1035 §4.2.1: a minimum retransmission interval of 2–5 s | `dns_tries`: four queries in the 5 s an app allows, all to the first resolver. The second resolver's turn comes at 7 s. |
| The first SYN is repeated after 3 s | RFC 6298 §2.1: 1 s (a SHOULD; 3 s is allowed) | `syn_tries`: three SYNs in the 8 s an app allows, where the RFC's timing gives four. |
| The wait between SYNs is not lengthened | RFC 6298 §5.5: doubling it is a MUST | SYNs at 0, 3 and 6 s. |
| The first sequence number comes from a counter that starts at 6510 and steps with a half-second tick | RFC 9293 §3.4.1: a 4 µs clock is a MUST, and adding a keyed hash of the connection a SHOULD | The number is easy to guess. `LWIP_HOOK_TCP_ISN` is the place to supply one. |

An app gives up on a connection after 8 s, where RFC 9293 §3.8.3 has TCP itself try for at least 3 minutes. That is allowed: the application may close sooner.

## Seen on the handset and not explained

Measured on 2026-11-25 with an image built before upstream's Bluetooth change (upstream `2839ee3` with the fixed-address and DHCP branches). Neither is modelled, because the cause is not known.

- The phone restarts by itself at 327–329 s of uptime (seen twice). `wrap_seconds`: Radio's clock comes round at 330 s. Whether that is the cause, and whether it is new, has not been established.
- After one join that reached DHCP and failed within seconds, every later join failed at step 6 until a reboot, and the battery reading was 0 mV.

## What is not modelled

- The inside of steps 1 and 2: registers, the WMT exchange, firmware download, and Bluetooth, which `radio.rs` now starts with the Wi-Fi firmware and stops when a join fails (`bt.rs`).
- The auto-connector (`runtime/open/wifi.rs`), which scans and joins on its own and shares the radio with a join asked for over USB.
- Hosting a network for Violet Relay, open networks, and group key renewal.
- Lease renewal, which is in the DHCP model, and anything after the TCP handshake: TLS and HTTP.
- What an access point does beyond answering or not.

## Sources

The RFCs are in [`docs/references`](../../references/README.md), unmodified. That page also says where to get what cannot be kept here: IEEE Std 802.11, which may not be redistributed, and NIST SP 800-97. IEEE 802.11 is cited above by the name of the procedure, not by clause number: the text was not to hand to check numbers against.
