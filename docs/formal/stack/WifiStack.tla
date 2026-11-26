----------------------------- MODULE WifiStack -----------------------------
(* The Moto G Pure's Wi-Fi path from "radio off" to an open TCP connection, as
   targets/moto-g-pure runs it: power and firmware, scan, IEEE 802.11
   authentication and association, the WPA2 4-way handshake, DHCP, then an
   app's ARP, DNS and TCP exchanges. README.md in this directory maps every
   step to its code and its standard.

   One exchange is in the air at a time, as in the code: Radio is one thread.
   `flight` is where its frame is and `left` how many more times its sender
   will send it. Time is not modelled: a wait runs out only when nothing more
   can arrive. WifiStack.lean has the arithmetic of the real time limits. *)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Aps,         \* access points carrying the network's name
  Heard,       \* those whose beacons can reach the scan (a channel it listens on)
  Reach,       \* those that hear the phone and that the phone hears
  ScanMisses,  \* TRUE: a scan may miss any beacon
  KeyOk,       \* the phone holds the network's key
  Faults,      \* TRUE: power and firmware start may fail
  Losses,      \* exchanges the air may drop
  Deauths,     \* times an access point may throw the phone off
  ApTries,     \* times an access point sends handshake message 1, and message 3
  DhcpTries, ArpTries, DnsTries, SynTries

ASSUME /\ Heard \subseteq Aps /\ Reach \subseteq Aps
       /\ {ScanMisses, KeyOk, Faults} \subseteq BOOLEAN
       /\ {Losses, Deauths, ApTries, DhcpTries, ArpTries, DnsTries, SynTries} \subseteq Nat

VARIABLES
  st,      \* how far the join has got
  code,    \* why a join failed: the step number moto-package reports
  radio,   \* the connectivity domain's power
  seen,    \* access points the scan found
  tried,   \* those the join has tried
  ap,      \* the one being tried or joined
  last,    \* the failure of the last one tried
  flight, left,
  lost, kicks,
  authed, assoc, keyed, lease,   \* 802.11 states 2, 3 and 4, and a DHCP lease
  re,      \* rejoining after a lost link, firmware still running
  m1s,     \* handshake messages received before the keys
  waited,  \* DHCP went unanswered to the end of its wait
  app, gw, host                  \* one app connection: ARP, DNS, TCP

vars == <<st, code, radio, seen, tried, ap, last, flight, left, lost, kicks,
          authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

Stages == {"off", "power", "firmware", "scan", "pick", "auth", "assoc",
           "m1", "m3", "dhcp", "online", "failed"}
Codes == {6, 37, 47, 48, 49, 52, 53, 57}
AppBusy == app \in {"arp", "dns", "syn"}

Init ==
  /\ st = "off" /\ code = 0 /\ radio = "off"
  /\ seen = {} /\ tried = {} /\ ap = "none" /\ last = 47
  /\ flight = "none" /\ left = 0 /\ lost = 0 /\ kicks = 0
  /\ authed = FALSE /\ assoc = FALSE /\ keyed = FALSE /\ lease = FALSE
  /\ re = FALSE /\ m1s = 0 /\ waited = FALSE
  /\ app = "idle" /\ gw = FALSE /\ host = FALSE

(* A failed join: Radio shuts the chip down and the supervisor cuts its power
   (radio.rs: axi.shutdown, request 0x4054). *)
Down(c) ==
  /\ st' = "failed" /\ code' = c /\ radio' = "off"
  /\ flight' = "none" /\ left' = 0
  /\ authed' = FALSE /\ assoc' = FALSE /\ keyed' = FALSE /\ lease' = FALSE

(* radio::relink: the same access point again, with nothing kept of the old link. *)
Again ==
  /\ st' = "auth" /\ flight' = "none" /\ left' = 1
  /\ authed' = FALSE /\ assoc' = FALSE /\ keyed' = FALSE /\ lease' = FALSE
  /\ UNCHANGED <<code, radio>>

Fail(c) == IF re THEN Again ELSE Down(c)

----------------------------------------------------------------------------
(* Power, firmware and scan. *)

Join ==
  /\ st = "off"
  /\ st' = "power" /\ radio' = "on"
  /\ UNCHANGED <<code, seen, tried, ap, last, flight, left, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

Started ==
  /\ st \in {"power", "firmware"}
  /\ st' = IF st = "power" THEN "firmware" ELSE "scan"
  /\ UNCHANGED <<code, radio, seen, tried, ap, last, flight, left, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

(* 6 stands for every power failure and 37 for every firmware one. *)
Fault ==
  /\ Faults /\ st \in {"power", "firmware"}
  /\ Down(IF st = "power" THEN 6 ELSE 37)
  /\ UNCHANGED <<seen, tried, ap, last, lost, kicks, re, m1s, waited, app, gw, host>>

Scan ==
  /\ st = "scan"
  /\ \E s \in SUBSET Heard :
       /\ ScanMisses \/ s = Heard
       /\ seen' = s
       /\ IF s = {}
          THEN Down(49)
          ELSE /\ st' = "pick"
               /\ UNCHANGED <<code, radio, flight, left, authed, assoc, keyed, lease>>
  /\ tried' = {}
  /\ UNCHANGED <<ap, last, lost, kicks, re, m1s, waited, app, gw, host>>

(* Every access point found is tried once, in no order the model can rely on:
   the code takes 2.4 GHz first and does not know which is nearest. *)
Pick ==
  /\ st = "pick"
  /\ IF seen \ tried = {}
     THEN Down(last) /\ UNCHANGED <<tried, ap>>
     ELSE \E a \in seen \ tried :
            /\ ap' = a /\ tried' = tried \cup {a}
            /\ st' = "auth" /\ left' = 1
            /\ UNCHANGED <<code, radio, flight, authed, assoc, keyed, lease>>
  /\ UNCHANGED <<seen, last, lost, kicks, re, m1s, waited, app, gw, host>>

----------------------------------------------------------------------------
(* An exchange the phone starts: a request, then an answer. *)

PhoneAsks ==
  /\ st \in {"auth", "assoc", "dhcp"} \/ (st = "online" /\ AppBusy)
  /\ flight = "none" /\ left > 0
  /\ flight' = "req" /\ left' = left - 1
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

(* The access point, the DHCP server, the router, the resolver or the web
   server answers. An access point out of reach never does. *)
Answered ==
  /\ st \in {"auth", "assoc", "dhcp"} \/ (st = "online" /\ AppBusy)
  /\ flight = "req"
  /\ flight' = IF st \in {"auth", "assoc"} /\ ap \notin Reach THEN "none" ELSE "rep"
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, left, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

(* The air drops the frame in flight, after whatever the chip itself retried. *)
Lose ==
  /\ flight # "none" /\ lost < Losses
  /\ flight' = "none" /\ lost' = lost + 1
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, left, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

----------------------------------------------------------------------------
(* IEEE 802.11 authentication and association: one request each, one wait. *)

AuthDone ==
  /\ st = "auth" /\ flight = "rep"
  /\ authed' = TRUE /\ st' = "assoc" /\ left' = 1 /\ flight' = "none"
  /\ UNCHANGED <<code, radio, seen, tried, ap, last, lost, kicks,
                 assoc, keyed, lease, re, m1s, waited, app, gw, host>>

AssocDone ==
  /\ st = "assoc" /\ flight = "rep"
  /\ assoc' = TRUE /\ st' = "m1" /\ left' = ApTries /\ flight' = "none" /\ m1s' = 0
  /\ UNCHANGED <<code, radio, seen, tried, ap, last, lost, kicks,
                 authed, keyed, lease, re, waited, app, gw, host>>

(* The 700 ms wait ran out: on to the next access point (47 after no
   authentication answer, 48 after no association answer). *)
NoAnswer ==
  /\ st \in {"auth", "assoc"} /\ flight = "none" /\ left = 0
  /\ last' = IF st = "auth" THEN 47 ELSE 48
  /\ authed' = FALSE /\ assoc' = FALSE
  /\ IF re THEN st' = "auth" /\ left' = 1 ELSE st' = "pick" /\ left' = 0
  /\ UNCHANGED <<code, radio, seen, tried, ap, flight, lost, kicks,
                 keyed, lease, re, m1s, waited, app, gw, host>>

----------------------------------------------------------------------------
(* The 4-way handshake. The access point sends messages 1 and 3 and repeats
   each until answered; the phone answers with 2 and 4. *)

ApSendsKey ==
  /\ st \in {"m1", "m3"} /\ flight = "none" /\ left > 0
  /\ flight' = "req" /\ left' = left - 1
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

GotM1 ==
  /\ st = "m1" /\ flight = "req"
  /\ m1s' = m1s + 1 /\ flight' = "rep"
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, left, lost, kicks,
                 authed, assoc, keyed, lease, re, waited, app, gw, host>>

(* Message 2 proves the phone has the key. Without it the access point goes
   on repeating message 1. *)
ApGotM2 ==
  /\ st = "m1" /\ flight = "rep"
  /\ flight' = "none"
  /\ IF KeyOk THEN st' = "m3" /\ left' = ApTries ELSE UNCHANGED <<st, left>>
  /\ UNCHANGED <<code, radio, seen, tried, ap, last, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, app, gw, host>>

(* Message 3: the supplicant installs the keys and the join moves on to DHCP. *)
GotM3 ==
  /\ st = "m3" /\ flight = "req"
  /\ keyed' = TRUE /\ st' = "dhcp" /\ left' = DhcpTries /\ flight' = "none"
  /\ UNCHANGED <<code, radio, seen, tried, ap, last, lost, kicks,
                 authed, assoc, lease, re, m1s, waited, app, gw, host>>

(* wpa::authorize: three or more messages without keys reads as a wrong key
   (52), fewer as a handshake that timed out (57). *)
NoKeys ==
  /\ st \in {"m1", "m3"} /\ flight = "none" /\ left = 0
  /\ Fail(IF m1s >= 3 THEN 52 ELSE 57)
  /\ UNCHANGED <<seen, tried, ap, last, lost, kicks, re, m1s, waited, app, gw, host>>

----------------------------------------------------------------------------
(* DHCP, as one exchange. docs/formal/dhcp models it in full. *)

Leased ==
  /\ st = "dhcp" /\ flight = "rep"
  /\ lease' = TRUE /\ st' = "online" /\ re' = FALSE /\ flight' = "none" /\ left' = 0
  /\ UNCHANGED <<code, radio, seen, tried, ap, last, lost, kicks,
                 authed, assoc, keyed, m1s, waited, app, gw, host>>

NoLease ==
  /\ st = "dhcp" /\ flight = "none" /\ left = 0
  /\ waited' = TRUE
  /\ Fail(53)
  /\ UNCHANGED <<seen, tried, ap, last, lost, kicks, re, m1s, app, gw, host>>

(* The access point sends a deauthentication. During the handshake the phone
   does not look for one and waits on; during DHCP the join ends at once;
   once online Radio rejoins and the apps' connections are reset. *)
Deauth ==
  /\ assoc /\ kicks < Deauths
  /\ kicks' = kicks + 1
  /\ CASE st \in {"m1", "m3"} ->
            /\ flight' = "none" /\ left' = 0
            /\ UNCHANGED <<st, code, radio, authed, assoc, keyed, lease, re, app, gw, host>>
       [] st = "dhcp" ->
            Fail(53) /\ UNCHANGED <<re, app, gw, host>>
       [] st = "online" ->
            /\ Again /\ re' = TRUE /\ gw' = FALSE /\ host' = FALSE
            /\ app' = IF app \in {"idle", "error"} THEN app ELSE "error"
       [] OTHER -> FALSE
  /\ UNCHANGED <<seen, tried, ap, last, lost, m1s, waited>>

----------------------------------------------------------------------------
(* One app connection (ip/sockets.c): the router's address by ARP, the
   server's by DNS, then the TCP handshake. *)

AppStarts ==
  /\ st = "online" /\ app = "idle" /\ flight = "none"
  /\ app' = "arp" /\ left' = ArpTries
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, flight, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, gw, host>>

AppStep ==
  /\ st = "online" /\ AppBusy /\ flight = "rep"
  /\ flight' = "none"
  /\ CASE app = "arp" -> app' = "dns" /\ gw' = TRUE /\ left' = DnsTries /\ UNCHANGED host
       [] app = "dns" -> app' = "syn" /\ host' = TRUE /\ left' = SynTries /\ UNCHANGED gw
       [] app = "syn" -> app' = "open" /\ left' = 0 /\ UNCHANGED <<gw, host>>
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited>>

AppGivesUp ==
  /\ st = "online" /\ AppBusy /\ flight = "none" /\ left = 0
  /\ app' = "error"
  /\ UNCHANGED <<st, code, radio, seen, tried, ap, last, flight, left, lost, kicks,
                 authed, assoc, keyed, lease, re, m1s, waited, gw, host>>

----------------------------------------------------------------------------
Next ==
  \/ Join \/ Started \/ Fault \/ Scan \/ Pick
  \/ PhoneAsks \/ Answered \/ Lose
  \/ AuthDone \/ AssocDone \/ NoAnswer
  \/ ApSendsKey \/ GotM1 \/ ApGotM2 \/ GotM3 \/ NoKeys
  \/ Leased \/ NoLease \/ Deauth
  \/ AppStarts \/ AppStep \/ AppGivesUp

Spec == Init /\ [][Next]_vars
FairSpec == Spec /\ WF_vars(Next)

----------------------------------------------------------------------------
(* What must hold in every state. *)

TypeOK ==
  /\ st \in Stages /\ code \in Codes \cup {0} /\ radio \in {"off", "on"}
  /\ seen \subseteq Aps /\ tried \subseteq Aps /\ ap \in Aps \cup {"none"}
  /\ last \in {47, 48} /\ flight \in {"none", "req", "rep"}
  /\ left \in Nat /\ lost \in 0..Losses /\ kicks \in 0..Deauths /\ m1s \in Nat
  /\ {authed, assoc, keyed, lease, re, waited, gw, host} \subseteq BOOLEAN
  /\ app \in {"idle", "arp", "dns", "syn", "open", "error"}

(* IEEE 802.11 states in order: keys only on an association, an association
   only after authentication, and both with an access point in reach that
   the scan found. *)
StatesInOrder ==
  /\ keyed => assoc
  /\ assoc => authed
  /\ authed => ap \in seen \cap Reach
  /\ seen \subseteq Heard /\ tried \subseteq seen
  /\ ap # "none" => ap \in tried

(* The IEEE 802.1X controlled port: no IP frame before the keys are in.
   DHCP, ARP, DNS and TCP all wait for them. *)
NoIpBeforeKeys ==
  /\ st \in {"dhcp", "online"} => keyed
  /\ lease => keyed
  /\ st = "online" <=> lease

(* An app's connection rests on every layer below it. *)
Layered ==
  /\ app \in {"arp", "dns", "syn", "open"} => st = "online"
  /\ app \in {"dns", "syn", "open"} => gw
  /\ app \in {"syn", "open"} => host

(* The key is needed: no keys, and so no address and no connection, without it. *)
KeyNeeded == keyed => KeyOk

(* A failed join leaves the chip off and nothing in use. *)
FailedIsOff ==
  /\ st = "failed" => radio = "off" /\ ~authed /\ ~assoc /\ ~keyed /\ ~lease /\ app = "idle"
  /\ st \notin {"off", "failed"} => radio = "on"
  /\ st = "failed" <=> code # 0

(* Each failure number comes from its own step. *)
CodeFits ==
  st = "failed" =>
    /\ code \in {6, 37} => Faults /\ seen = {}
    /\ code = 49 => seen = {}
    /\ code \in {47, 48} => seen # {} /\ tried = seen
    /\ code \in {52, 53, 57} => ap \in seen \cap Reach
    /\ code = 52 => m1s >= 3
    /\ code = 57 => m1s < 3

(* Nothing is in the air unless someone is waiting for it. *)
OneExchange ==
  flight # "none" => st \in {"auth", "assoc", "m1", "m3", "dhcp"} \/ (st = "online" /\ AppBusy)

----------------------------------------------------------------------------
(* What holds only on some networks. Each .cfg file says which it expects. *)

NoFailure == st # "failed" /\ app # "error"

(* With an access point in reach, the join does not end for want of one. *)
ReachableMeansJoined == (st = "failed" /\ Reach # {}) => code \notin {47, 48, 49}

(* One dropped exchange can end a join only at authentication or association,
   the two steps with a single try; it never fails an app's connection. *)
OneLossOnlyHurtsTheStart == (st = "failed" => code \in {47, 48}) /\ app # "error"

(* "No address" means DHCP went unanswered to the end of its wait. *)
NoAddressMeansWaited == (st = "failed" /\ code = 53) => waited

WrongKeyFails == (st = "failed") => code = 52

Connects == <>(app = "open")
ReportsWrongKey == <>(st = "failed" /\ code = 52)
=============================================================================
