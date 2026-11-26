----------------------------- MODULE DhcpClient -----------------------------
(***************************************************************************)
(* The DHCP client of RFC 2131 (docs/references/rfc2131.txt, section 4.4   *)
(* and Figure 5) as the Moto IP client runs it, with one server, on a      *)
(* network that loses, delays, repeats and reorders messages.              *)
(*                                                                         *)
(* The code is targets/moto-g-pure/ip/client.c over lwIP 2.2.1.            *)
(* targets/moto-g-pure/ip/test_dhcp.c holds that code to the states and    *)
(* steps below. DhcpClient.lean proves the same safety properties for a    *)
(* lease of any length; TLC checks them here together with the network,    *)
(* the server and the join, for one lease length.                          *)
(*                                                                         *)
(* INIT is folded into SELECTING, as in lwIP. INIT-REBOOT and REBOOTING    *)
(* are left out: the phone never asks for a remembered address.            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Lease,          \* how long a lease lasts, in ticks
  ConflictCheck,  \* TRUE: check the address before using it (RFC 2131 3.1 step 5)
  ProxyArp,       \* TRUE: the access point answers ARP for the address it leased
  InUse           \* TRUE: another device already holds the address

ASSUME Lease \in Nat /\ Lease >= 8
ASSUME ConflictCheck \in BOOLEAN /\ ProxyArp \in BOOLEAN /\ InUse \in BOOLEAN

\* RFC 2131 4.4.5: T1 and T2 default to 0.5 and 0.875 of the lease.
T1 == Lease \div 2
T2 == (Lease * 7) \div 8

VARIABLES
  phase,  \* the client's state
  age,    \* ticks since the lease in use was granted
  net,    \* every message sent so far
  join    \* station_net_join: "waiting", "joined" or "failed"

vars == <<phase, age, net, join>>

Phases == {"OFF", "SELECTING", "REQUESTING", "CHECKING", "BOUND", "RENEWING", "REBINDING"}
Types == {"DISCOVER", "OFFER", "REQUEST", "ACK", "NAK", "DECLINE"}

\* `current` says whether a message carries the transaction id ('xid') of the
\* exchange the client has open (RFC 2131 4.1).
Message == [type : Types, current : BOOLEAN]
Msg(t) == [type |-> t, current |-> TRUE]

\* A new exchange takes a new id, so everything sent before it is stale.
Stale(msgs) == {[type |-> m.type, current |-> FALSE] : m \in msgs}

UsesAddress == phase \in {"BOUND", "RENEWING", "REBINDING"}

Init ==
  /\ phase = "SELECTING"
  /\ age = 0
  /\ net = {Msg("DISCOVER")}
  /\ join = "waiting"

-----------------------------------------------------------------------------
\* The client.

Restart ==
  /\ phase' = "SELECTING"
  /\ age' = 0
  /\ net' = Stale(net) \cup {Msg("DISCOVER")}

NewRequest(p) ==
  /\ phase' = p
  /\ net' = Stale(net) \cup {Msg("REQUEST")}

Bind ==
  /\ phase' = "BOUND"
  /\ age' = 0
  /\ join' = IF join = "waiting" THEN "joined" ELSE join

RecvOffer ==
  /\ phase = "SELECTING"
  /\ Msg("OFFER") \in net
  /\ phase' = "REQUESTING"
  /\ net' = net \cup {Msg("REQUEST")}
  /\ UNCHANGED <<age, join>>

RecvAck ==
  /\ Msg("ACK") \in net
  /\ \/ /\ phase = "REQUESTING"
        /\ IF ConflictCheck
             THEN phase' = "CHECKING" /\ UNCHANGED <<age, join>>
             ELSE Bind
     \/ /\ phase \in {"RENEWING", "REBINDING"}
        /\ Bind
  /\ UNCHANGED net

\* RFC 2131 3.1 step 5 and 3.2.3: a refusal restarts the configuration.
RecvNak ==
  /\ Msg("NAK") \in net
  /\ phase \in {"REQUESTING", "RENEWING", "REBINDING"}
  /\ Restart
  /\ UNCHANGED join

\* RFC 2131 3.1 step 5: with no answer to its request after retrying, the
\* client starts again.
RequestTimeout ==
  /\ phase = "REQUESTING"
  /\ Restart
  /\ UNCHANGED join

\* The address check ends with no ARP packet naming the address.
CheckClear ==
  /\ phase = "CHECKING"
  /\ ~InUse /\ ~ProxyArp
  /\ Bind
  /\ UNCHANGED net

\* RFC 5227 2.1.1: any ARP packet naming the address means it is in use, and
\* RFC 2131 3.1 step 5: the client declines it and starts again. The answer
\* is taken never to be lost.
CheckConflict ==
  /\ phase = "CHECKING"
  /\ InUse \/ ProxyArp
  /\ phase' = "SELECTING"
  /\ age' = 0
  /\ net' = Stale(net \cup {Msg("DECLINE")}) \cup {Msg("DISCOVER")}
  /\ UNCHANGED join

\* RFC 2131 4.4.5: renew at T1, ask any server at T2, stop at the lease's end.
Tick ==
  /\ UsesAddress
  /\ UNCHANGED join
  /\ IF age + 1 >= Lease
       THEN Restart
       ELSE /\ age' = age + 1
            /\ IF age + 1 >= T2 /\ phase # "REBINDING"
                 THEN NewRequest("REBINDING")
                 ELSE IF age + 1 >= T1 /\ phase = "BOUND"
                        THEN NewRequest("RENEWING")
                        ELSE UNCHANGED <<phase, net>>

\* station_net_join's wait runs out: it takes the interface down.
GiveUp ==
  /\ join = "waiting"
  /\ join' = "failed"
  /\ phase' = "OFF"
  /\ age' = 0
  /\ UNCHANGED net

-----------------------------------------------------------------------------
\* The server, which cannot tell a stale request from a current one. It
\* answers a request once, either way.

Has(t, c) == [type |-> t, current |-> c] \in net
Send(t, c) == net' = net \cup {[type |-> t, current |-> c]}

ServerOffer(c) ==
  /\ Has("DISCOVER", c)
  /\ Send("OFFER", c)
  /\ UNCHANGED <<phase, age, join>>

ServerAck(c) ==
  /\ Has("REQUEST", c)
  /\ ~Has("NAK", c)
  /\ Send("ACK", c)
  /\ UNCHANGED <<phase, age, join>>

ServerNak(c) ==
  /\ Has("REQUEST", c)
  /\ ~Has("ACK", c)
  /\ Send("NAK", c)
  /\ UNCHANGED <<phase, age, join>>

-----------------------------------------------------------------------------

Next ==
  \/ RecvOffer \/ RecvAck \/ RecvNak \/ RequestTimeout
  \/ CheckClear \/ CheckConflict \/ Tick \/ GiveUp
  \/ \E c \in BOOLEAN : ServerOffer(c) \/ ServerAck(c) \/ ServerNak(c)

Spec == Init /\ [][Next]_vars

\* A network that delivers an answer in the end, to a client that does not
\* give up on every request just before its answer arrives, and a server
\* that does not refuse for ever. Nothing obliges the wait to run out.
Fairness ==
  /\ WF_vars(RecvOffer) /\ SF_vars(RecvAck) /\ WF_vars(RecvNak)
  /\ WF_vars(CheckClear) /\ WF_vars(CheckConflict)
  /\ WF_vars(ServerOffer(TRUE))
  /\ SF_vars(ServerAck(TRUE))

FairSpec == Spec /\ Fairness

-----------------------------------------------------------------------------
\* What must always hold.

TypeOK ==
  /\ phase \in Phases
  /\ age \in 0..Lease
  /\ net \subseteq Message
  /\ join \in {"waiting", "joined", "failed"}

\* RFC 2131 4.4.5: no use of the address once its lease has run out.
LeaseLive == UsesAddress => age < Lease

\* No renewal before T1 and no rebinding before T2.
TimersRespected ==
  /\ phase = "RENEWING" => age >= T1
  /\ phase = "REBINDING" => age >= T2

\* RFC 2131 4.1: only an acknowledgement of the open exchange binds; one left
\* over from an exchange the client abandoned does not.
BoundByCurrentAck == phase = "BOUND" => Msg("ACK") \in net

AddressWasGranted == UsesAddress => \E m \in net : m.type = "ACK"

\* A failed join leaves the interface down.
FailedIsDown == join = "failed" => phase = "OFF"

\* What the address check is for. It holds with the check on. With the check
\* off it holds only where no other device holds an address the server gives
\* out: NoCheckInUse.cfg shows it broken.
NoDuplicateAddress == UsesAddress => ~InUse

\* Holds where the check is on and something always answers for the address:
\* the join cannot succeed (CheckProxyArp.cfg).
NeverJoined == join # "joined"

\* A join reports success only as the address comes into use.
JoinedMeansBound == [][join = "waiting" /\ join' = "joined" => phase' = "BOUND"]_vars

\* After a failed join the client sends nothing.
ClientSent == {m \in net : m.type \in {"DISCOVER", "REQUEST", "DECLINE"}}
Quiet == [][join = "failed" => ClientSent' = ClientSent]_vars

\* With a fair network and server, a join that is not cut short succeeds.
Liveness == <>(join # "waiting")

=============================================================================
