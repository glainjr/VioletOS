# The DHCP client, modelled

The phone's DHCP client is lwIP 2.2.1 driven by `targets/moto-g-pure/ip/client.c`. This directory states what that client must do, from [RFC 2131](../../references/rfc2131.txt) and [RFC 5227](../../references/rfc5227.txt), in two machine-checked forms. A host test holds the code to them.

| File | What it is | Checked by |
|---|---|---|
| `DhcpClient.lean` | The client's states as a function, with proofs that hold for every sequence of events and every lease length; the arithmetic of lwIP's lease timers and retry schedule. | `lean` |
| `DhcpClient.tla` and the `.cfg` files | The same states with a server, a lossy network and the join's time limit, for one lease length. | TLC |
| `targets/moto-g-pure/ip/test_dhcp.c` | The real client against a scripted network. At every poll it checks that the code is in a state the models have, moved there by a step they allow, with their invariants true. | `make test-moto-dhcp` |

`make check-dhcp-model` runs Lean and TLC. Lean comes from [elan](https://lean-lang.org/install); the version is pinned in `lean-toolchain`. TLC needs `tlc` on `PATH`, or `TLA2TOOLS` set to `tla2tools.jar` from the [TLA+ releases](https://github.com/tlaplus/tlaplus/releases) and Java (`JAVA` names the `java` to run, if it is not the one on `PATH`). The model was last checked with TLC 2.19, release v1.7.4. Neither runs in CI.

The models are proofs about the models. What ties them to the code is the test, and it covers only the paths its cases reach.

## What is established

| Rule | Source | Lean | TLA+ | Test |
|---|---|---|---|---|
| An address is used only under a lease that has not run out | RFC 2131 §4.4.5 | `sound_run` (`live`) | `LeaseLive` | `LeaseLive` |
| No address without a server acknowledging the open exchange; answers to abandoned exchanges are discarded | RFC 2131 §4.1, §4.4.1 | `no_address_without_grant` | `BoundByCurrentAck`, `AddressWasGranted` | `AddressWasGranted` |
| Renewal no earlier than T1, rebinding no earlier than T2, T1 before T2 before the end | RFC 2131 §4.4.5 | `sound_run`, `renew_before_rebind`, `timers_ordered` | `TimersRespected` | `TimersRespected`, to lwIP's one-minute grain |
| Only the steps of Figure 5 | RFC 2131 §4.4 | `step` | `Next` | `allowed` |
| A failed join takes the interface down and sends nothing more | `client.c` | | `FailedIsDown`, `Quiet` | `check_gave_up` |
| With a fair network and server, a join that is not cut short succeeds | | | `Liveness` (`Phone.cfg`, `Check.cfg`) | the passing cases |
| With the address check on, something always answering for the address means no join | RFC 5227 §2.1.1 | `always_in_use_never_bound` | `NeverJoined` (`CheckProxyArp.cfg`) | measured before the check was turned off |
| With the address check off, a granted address is used at once | `lwipopts.h` | `grant_binds_at_once` | `RecvAck` | "address in use after 3 ms" |
| Four requests leave within the 30 s wait, the last at 14 s | lwIP `dhcp_discover` | `four_tries` | | "no answer" case |

## Where the phone departs from the RFCs

Nothing obsoletes RFC 2131: it is still the DHCP standard for IPv4, as the RFC Editor's index had it on 2026-11-25. Four later RFCs update it, and RFC 7844 adds a privacy profile for a client that, like this phone, joins under a random hardware address.

One departure is this project's choice. Every other one is how lwIP 2.2.1 behaves as shipped. They were there before the address check was turned off, and nothing here adds to them or changes them.

### Ours

| Departure | Effect |
|---|---|
| No check that the granted address is free: `LWIP_DHCP_DOES_ACD_CHECK 0` in `ip/lwipopts.h`. RFC 2131 §3.1 step 5 says a client SHOULD check. RFC 5227 §2.1 makes the check a MUST for a host that implements RFC 5227, which the phone now does not. | The phone does not notice another device holding its address. `NoCheckInUse.cfg` shows the invariant this gives up. In return a join does not spend 6–9 s checking, and works behind an access point that answers ARP for its clients. |

`client.c` also never uses INIT-REBOOT (RFC 2131 §3.2), so every join is a full exchange. That is allowed, and it is what RFC 7844 §3.3 recommends.

### lwIP 2.2.1, against RFC 2131

| Departure | RFC 2131 | Effect |
|---|---|---|
| No random 1–10 s wait before the first request | §4.4.1, a SHOULD | Faster joins. |
| Retries are not randomised, and start at 2 s where the RFC suggests 4 s | §4.1; the randomising is a MUST | `lwip_no_slower`. Phones that start together retry together. |
| A lease is counted from the acknowledgement's arrival, in whole minutes, where the RFC counts from the request's sending | §4.4.1 | The address can outlive its lease by the round trip. |
| A lease under 90 s, or of 91 days or more, is never renewed | §4.4.5: T1 before T2 before the end, a MUST | lwIP's 16-bit minute counters. `short_lease_not_renewed`, `long_lease_not_renewed`: the address is dropped and asked for again each minute, or after 45.5 days. |
| A refusal restarts the exchange with no pause | none: the RFC asks for a 10 s wait only after a client declines an address (§3.1 step 5) | A server that refuses everything is asked again as fast as the link allows: 15,000 requests in 30 s in the test. |

### lwIP 2.2.1, against the later RFCs

| RFC | What it asks of a client | lwIP 2.2.1 | Effect |
|---|---|---|---|
| [RFC 3396](../../references/rfc3396.txt) (2002, updates 2131): long options | Join the parts of an option that arrives as several options with the same code (§7, a MUST) | Reads the first and ignores the rest. If the first is not the length the whole option should have, it drops the message. | The "server splits an option" case: every offer dropped, no join. A server has no cause to split the options the phone reads, each a few bytes long. |
| [RFC 4361](../../references/rfc4361.txt) (2006, updates 2131): client identifiers | Send a client identifier that stays the same when the hardware address changes (§6.1, a MUST) | Sends none | The server knows the phone by its hardware address alone. For an address that is random, that is the point, and RFC 7844 §3.5 sets RFC 4361 aside for such clients. |
| RFC 5494 (2009, updates 2131): ARP numbers | Nothing: it is rules for IANA | | |
| [RFC 6842](../../references/rfc6842.txt) (2013, updates 2131): client identifier in replies | Discard a reply whose client identifier is not the one the client sends (§3, a MUST) | Does not look at the option in a reply | None with a server that follows the same RFC: the phone sends no identifier, so the server must return none. Replies are matched on transaction id and hardware address. |
| [RFC 7844](../../references/rfc7844.txt) (2016): anonymity profile | Say nothing that names the device: no host name (§3.7), no vendor class (§3.10), no remembered address in DHCPDISCOVER (§3.3) | Sends none of them | `check_sent` in the test: the phone's hardware address is all the network learns of it. |
| | Send no option beyond message type, parameter request list and client identifier in DHCPDISCOVER (§3, a SHOULD NOT) | Also sends maximum message size (option 57) | The options and their order mark the client as lwIP. They do not tell one lwIP device from another. |
| | Options in random order, or else ascending (§3.1); the same for the parameter request list (§3.6); both a SHOULD | Fixed order, 53 57 55; the list is 1 3 28 6 | As above. |
