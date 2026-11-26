/-
The Moto G Pure's Wi-Fi path from "radio off" to an open TCP connection, as
targets/moto-g-pure runs it. README.md in this directory maps each step to its
code and its standard. Two parts:

  1. the order of the layers, for every sequence of events
  2. the arithmetic of the code's clocks, scan, retries and addresses

Check with `lean WifiStack.lean` in this directory: no output means every
theorem holds. These are proofs about this model, not about the code. Nothing
yet holds the radio code to it the way ip/test_dhcp.c holds the DHCP client to
docs/formal/dhcp.
-/
namespace WifiStack

/-! ## 1. The layers, in order

`handshake` is the WPA2 4-way handshake; `online` is a joined network with a
DHCP lease. IEEE 802.11 calls `authed` state 2, `assoc` state 3 and `keyed`
state 4. -/

inductive Stage where
  | off | power | firmware | scan | auth | assoc | handshake | dhcp | online | failed
  deriving DecidableEq, Repr

/-- One app connection (ip/sockets.c): ARP for the router, DNS, then TCP. -/
inductive App where
  | idle | arp | dns | syn | opened | error
  deriving DecidableEq, Repr

structure State where
  stage : Stage
  /-- The connectivity domain has power. -/
  radio : Bool
  authed : Bool
  assoc : Bool
  /-- Pairwise keys are installed: the IEEE 802.1X controlled port is open. -/
  keyed : Bool
  lease : Bool
  /-- Radio is rejoining after a lost link (radio::relink). -/
  rejoin : Bool
  app : App
  /-- The router's hardware address is known. -/
  gateway : Bool
  /-- The server's IP address is known. -/
  host : Bool
  /-- The step number of a failed join. -/
  code : Nat
  deriving Repr

def State.start : State := ⟨.off, false, false, false, false, false, false, .idle, false, false, 0⟩

/-- A failed join: the chip is shut down and its power cut. -/
def State.down (code : Nat) : State :=
  ⟨.failed, false, false, false, false, false, false, .idle, false, false, code⟩

/-- The same access point again, with nothing kept of the old link. -/
def State.again (s : State) : State :=
  { s with stage := .auth, authed := false, assoc := false, keyed := false, lease := false }

def State.fail (s : State) (code : Nat) : State :=
  if s.rejoin then s.again else State.down code

def App.busy : App → Bool
  | .arp | .dns | .syn => true
  | _ => false

inductive Event where
  /-- A join request: the supervisor powers the connectivity domain. -/
  | join
  | powered
  | booted
  /-- Power or firmware start failed. -/
  | fault (code : Nat)
  /-- The scan ended; `found` says whether it found the network. -/
  | scanned (found : Bool)
  | authOk
  | assocOk
  /-- No answer within 700 ms; `more` says whether another access point is left. -/
  | noAnswer (more : Bool)
  /-- Handshake message 3 was accepted and the keys installed. -/
  | keys
  /-- The handshake did not finish: 52 or 57. -/
  | noKeys (code : Nat)
  | leased
  | noLease
  /-- The access point sent a deauthentication. -/
  | deauth
  | appStart
  | arpOk
  | dnsOk
  | synAck
  | appTimeout

def step (s : State) : Event → State
  | .join =>
    if s.stage = .off ∨ s.stage = .failed then { State.start with stage := .power, radio := true }
    else s
  | .powered => if s.stage = .power then { s with stage := .firmware } else s
  | .booted => if s.stage = .firmware then { s with stage := .scan } else s
  | .fault code => if s.stage = .power ∨ s.stage = .firmware then State.down code else s
  | .scanned found =>
    if s.stage = .scan then
      if found then { s with stage := .auth } else State.down 49
    else s
  | .authOk => if s.stage = .auth then { s with stage := .assoc, authed := true } else s
  | .assocOk => if s.stage = .assoc then { s with stage := .handshake, assoc := true } else s
  | .noAnswer more =>
    if s.stage = .auth ∨ s.stage = .assoc then
      if more ∨ s.rejoin then s.again
      else State.down (if s.stage = .auth then 47 else 48)
    else s
  | .keys => if s.stage = .handshake then { s with stage := .dhcp, keyed := true } else s
  | .noKeys code => if s.stage = .handshake then s.fail code else s
  | .leased =>
    if s.stage = .dhcp then { s with stage := .online, lease := true, rejoin := false } else s
  | .noLease => if s.stage = .dhcp then s.fail 53 else s
  | .deauth =>
    -- During the handshake the phone does not look for one and waits on.
    if s.stage = .dhcp then s.fail 53
    else if s.stage = .online then
      { s.again with rejoin := true, gateway := false, host := false,
                     app := if s.app.busy ∨ s.app = .opened then .error else s.app }
    else s
  | .appStart => if s.stage = .online ∧ s.app = .idle then { s with app := .arp } else s
  | .arpOk =>
    if s.stage = .online ∧ s.app = .arp then { s with app := .dns, gateway := true } else s
  | .dnsOk =>
    if s.stage = .online ∧ s.app = .dns then { s with app := .syn, host := true } else s
  | .synAck => if s.stage = .online ∧ s.app = .syn then { s with app := .opened } else s
  | .appTimeout => if s.stage = .online ∧ s.app.busy then { s with app := .error } else s

def run (events : List Event) : State := events.foldl step State.start

/-- What holds of every state the phone can reach. -/
structure Sound (s : State) : Prop where
  /-- IEEE 802.11: association only after authentication, keys only on an association. -/
  assoc_authed : s.assoc = true → s.authed = true
  keyed_assoc : s.keyed = true → s.assoc = true
  /-- Each step is taken in the state the one before it left. -/
  assoc_stage : s.stage = .assoc → s.authed = true
  handshake_stage : s.stage = .handshake → s.assoc = true
  /-- IEEE 802.1X: no IP frame before the keys. DHCP runs only on a keyed link. -/
  dhcp_keyed : s.stage = .dhcp → s.keyed = true
  lease_keyed : s.lease = true → s.keyed = true
  online_lease : s.stage = .online → s.lease = true
  /-- An app's connection rests on a joined network, then ARP, then DNS. -/
  app_online : s.app.busy = true ∨ s.app = .opened → s.stage = .online
  dns_gateway : s.app = .dns ∨ s.app = .syn ∨ s.app = .opened → s.gateway = true
  syn_host : s.app = .syn ∨ s.app = .opened → s.host = true
  /-- Without power nothing is in use. -/
  off_idle : s.radio = false → s.authed = false ∧ s.lease = false ∧ s.app = .idle
  failed_off : s.stage = .failed → s.radio = false

theorem sound_start : Sound State.start := by
  constructor <;> simp [State.start, App.busy]

theorem sound_down (code : Nat) : Sound (State.down code) := by
  constructor <;> simp [State.down, App.busy]

theorem sound_again (s : State) (h : Sound s) (hs : s.stage ≠ .online)
    (hrad : s.radio = true) : Sound s.again := by
  obtain ⟨_, _, _, _, _, _, _, h6, h7, h8, _, _⟩ := h
  constructor <;> simp_all [State.again]

theorem sound_fail (s : State) (code : Nat) (h : Sound s) (hs : s.stage ≠ .online)
    (hrad : s.radio = true) : Sound (s.fail code) := by
  unfold State.fail
  split
  · exact sound_again s h hs hrad
  · exact sound_down code

/-- A state past `off` and `failed` has power. -/
def Powered (s : State) : Prop := s.stage ≠ .off → s.stage ≠ .failed → s.radio = true

theorem powered_step (s : State) (e : Event) (h : Powered s) : Powered (step s e) := by
  unfold Powered at *
  cases e with
  | noAnswer more =>
    simp only [step]; split
    · rename_i hs
      split
      · intro _ _
        rcases hs with hs | hs <;> exact h (by simp [hs]) (by simp [hs])
      · simp [State.down]
    · exact h
  | _ =>
    simp only [step, State.fail, State.again, State.down, State.start] <;>
      (repeat' split) <;> simp_all

theorem sound_step (s : State) (e : Event) (h : Sound s) (hp : Powered s) :
    Sound (step s e) := by
  have hrad : s.stage ≠ .off → s.stage ≠ .failed → s.radio = true := hp
  cases e with
  | join =>
    simp only [step]; split
    · constructor <;> simp [State.start, App.busy]
    · exact h
  | fault code => simp only [step]; split <;> first | exact sound_down code | exact h
  | scanned found =>
    simp only [step]; split
    · split
      · obtain ⟨_, _, _, _, _, _, _, _, _, _, _, _⟩ := h
        constructor <;> simp_all
      · exact sound_down 49
    · exact h
  | noAnswer more =>
    simp only [step]; split
    · rename_i hs
      split
      · have hon : s.stage ≠ .online := by
          rcases hs with hs | hs <;> simp [hs]
        have hoff : s.stage ≠ .off ∧ s.stage ≠ .failed := by
          rcases hs with hs | hs <;> simp [hs]
        exact sound_again s h hon (hrad hoff.1 hoff.2)
      · exact sound_down _
    · exact h
  | noKeys code =>
    simp only [step]; split
    · rename_i hs
      exact sound_fail s code h (by simp [hs]) (hrad (by simp [hs]) (by simp [hs]))
    · exact h
  | noLease =>
    simp only [step]; split
    · rename_i hs
      exact sound_fail s 53 h (by simp [hs]) (hrad (by simp [hs]) (by simp [hs]))
    · exact h
  | deauth =>
    simp only [step]; split
    · rename_i hs
      exact sound_fail s 53 h (by simp [hs]) (hrad (by simp [hs]) (by simp [hs]))
    · split
      · rename_i hs
        obtain ⟨_, _, _, _, _, _, _, _, _, _, _, _⟩ := h
        have := hrad (by simp [hs]) (by simp [hs])
        constructor <;> simp_all [State.again] <;>
          (cases happ : s.app <;> simp_all [App.busy])
      · exact h
  | _ =>
    simp only [step]; split
    · rename_i hs
      obtain ⟨_, _, _, _, _, _, _, _, _, _, _, _⟩ := h
      constructor <;> simp_all [App.busy]
    · exact h

theorem run_invariant (events : List Event) : Sound (run events) ∧ Powered (run events) := by
  suffices ∀ s, Sound s → Powered s →
      Sound (events.foldl step s) ∧ Powered (events.foldl step s) from
    this _ sound_start (by simp [Powered, State.start])
  induction events with
  | nil => intro s h hp; exact ⟨h, hp⟩
  | cons e rest ih => intro s h hp; exact ih _ (sound_step s e h hp) (powered_step s e hp)

theorem sound_run (events : List Event) : Sound (run events) := (run_invariant events).1

/-- IEEE 802.1X, for every sequence of events: the phone runs DHCP, holds an
address or is online only with the pairwise keys installed. -/
theorem no_ip_before_keys (events : List Event)
    (h : (run events).stage = .dhcp ∨ (run events).stage = .online ∨ (run events).lease = true) :
    (run events).keyed = true := by
  have s := sound_run events
  rcases h with h | h | h
  · exact s.dhcp_keyed h
  · exact s.lease_keyed (s.online_lease h)
  · exact s.lease_keyed h

/-- An open TCP connection rests on every layer below it. -/
theorem open_needs_every_layer (events : List Event) (h : (run events).app = .opened) :
    (run events).radio = true ∧ (run events).authed = true ∧ (run events).assoc = true ∧
    (run events).keyed = true ∧ (run events).lease = true ∧
    (run events).gateway = true ∧ (run events).host = true := by
  have s := sound_run events
  have online := s.app_online (Or.inr h)
  have lease := s.online_lease online
  have keyed := s.lease_keyed lease
  have assoc := s.keyed_assoc keyed
  refine ⟨?_, s.assoc_authed assoc, assoc, keyed, lease, s.dns_gateway (by simp [h]),
    s.syn_host (by simp [h])⟩
  cases hr : (run events).radio
  · have := (s.off_idle hr).2.1; simp_all
  · rfl

/-- A failed join leaves the chip without power and nothing in use. -/
theorem failed_is_off (events : List Event) (h : (run events).stage = .failed) :
    (run events).radio = false ∧ (run events).authed = false ∧ (run events).lease = false ∧
    (run events).app = .idle := by
  have s := sound_run events
  have off := s.failed_off h
  exact ⟨off, s.off_idle off⟩

/-- The keys come only from handshake message 3. -/
theorem keyed_needs_keys_event (events : List Event) (h : ∀ e ∈ events, e ≠ .keys) :
    (run events).keyed = false := by
  suffices ∀ s : State, s.keyed = false → (events.foldl step s).keyed = false from
    this _ (by simp [State.start])
  induction events with
  | nil => intro s hs; exact hs
  | cons e rest ih =>
    intro s hs
    refine ih (fun e' he' => h e' (List.mem_cons_of_mem _ he')) _ ?_
    have hne : e ≠ .keys := h e (List.mem_cons_self ..)
    cases e <;> simp only [step, State.fail, State.again, State.down, State.start] <;>
      (repeat' split) <;> simp_all

/-- With the wrong key the handshake never yields keys, so the phone never
holds an address and no connection ever opens. -/
theorem wrong_key_never_online (events : List Event) (h : ∀ e ∈ events, e ≠ .keys) :
    (run events).lease = false ∧ (run events).stage ≠ .online ∧ (run events).app ≠ .opened := by
  have s := sound_run events
  have k := keyed_needs_keys_event events h
  have lease : (run events).lease = false := by
    cases hl : (run events).lease
    · rfl
    · have := s.lease_keyed hl; simp_all
  refine ⟨lease, ?_, ?_⟩
  · intro hon; have := s.online_lease hon; simp_all
  · intro hop
    have := s.online_lease (s.app_online (Or.inr hop)); simp_all

/-! ## 2. Radio's clock (radio_io.rs)

`now` is a 32-bit count of a 13 MHz clock. `expired(start, ms)` is
`now.wrapping_sub(start) >= ticks_per_ms * ms`. -/

def wrap : Nat := 4294967296
def ticksPerMs : Nat := 13000

/-- `radio_io::expired`, for a `start` and `now` below `wrap`. -/
def expired (start now ms : Nat) : Bool := (now + wrap - start) % wrap ≥ ticksPerMs * ms

/-- The count returns to where it was after 330 s. -/
theorem wrap_seconds : wrap / (ticksPerMs * 1000) = 330 := by decide

/-- A wait that is checked before the count comes round again gets the right
answer, wherever in the count it started. -/
theorem expired_exact (start elapsed ms : Nat) (hs : start < wrap) (he : elapsed < wrap) :
    expired start ((start + elapsed) % wrap) ms = decide (ticksPerMs * ms ≤ elapsed) := by
  unfold expired wrap ticksPerMs at *
  have : ((start + elapsed) % 4294967296 + 4294967296 - start) % 4294967296 = elapsed := by omega
  rw [this]

/-- The longest wait in Radio, 150 s, fits in the count. -/
theorem longest_wait_fits : ticksPerMs * 150000 < wrap := by decide

/-- No wait of 330.383 s or more can be asked for: the product overflows 32 bits. -/
theorem wait_too_long : wrap ≤ ticksPerMs * 330383 := by decide

/-- A wait first checked a whole turn of the count after it began looks as if
it had only just begun: the 330 s that passed are not seen. -/
theorem missed_after_a_turn (start ms : Nat) (hs : start < wrap) (hm : 0 < ms) :
    expired start ((start + wrap) % wrap) ms = false := by
  unfold expired wrap ticksPerMs at *
  have : ((start + 4294967296) % 4294967296 + 4294967296 - start) % 4294967296 = 0 := by
    have := hs; omega
  rw [this]
  simp; omega

/-! ## 3. The scan (passive_scan.rs)

A passive scan of 20 channels. The 120 in the request is taken to be the
milliseconds spent on each. -/

def scanned : List Nat := [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 36, 40, 44, 48, 149, 153, 157, 161, 165]
def dwellMs : Nat := 120
/-- Radio waits this long for the chip to report the scan done. -/
def scanLimitMs : Nat := 4500

theorem scanned_count : scanned.length = 20 := by decide

/-- 2.4 GHz channels 12 to 14 and the 5 GHz channels 52 to 144 are never
listened on: an access point there is not found. -/
theorem never_scanned :
    ∀ c ∈ [12, 13, 14, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144],
      c ∉ scanned := by decide

theorem scan_fits_its_limit : scanned.length * dwellMs ≤ scanLimitMs := by decide

/-- An access point beacons every 100 time units of 1024 µs. One stay on a
channel is long enough for one beacon and too short to be sure of two, so a
beacon lost in the air is an access point not found. -/
theorem one_beacon_per_stay : 100 * 1024 ≤ dwellMs * 1000 ∧ dwellMs * 1000 < 2 * (100 * 1024) := by
  decide

/-! ## 4. How long a join can take (radio.rs, wlan_axi.rs, wpa.rs, ip/client.c)

For each access point with the network's name: up to 1 s for the channel,
700 ms each for authentication, association and the chip's confirmation. Then
8 s for the handshake and 30 s for DHCP. Power and firmware start come before
and are not counted. `moto-package join-wifi` waits 85 s for the result. -/

def perApMs : Nat := 1000 + 700 + 700 + 700
def joinLimitMs (aps : Nat) : Nat := scanLimitMs + aps * perApMs + 8000 + 30000
def hostWaitMs : Nat := 85000

/-- The scan keeps up to 32 access points. With 14 or more of one name, each
taking its full time, the join outlasts the host's wait. -/
theorem join_fits_host_wait (aps : Nat) : joinLimitMs aps ≤ hostWaitMs ↔ aps ≤ 13 := by
  unfold joinLimitMs perApMs scanLimitMs hostWaitMs; omega

/-! ## 5. Retries above the radio (lwIP 2.2.1)

ip/sockets.c gives a name 5 s to resolve and a TCP connection 8 s to open. -/

/-- When lwIP sends the nth DNS query, in seconds: waits of 1, 1, 2 and 3 s,
then the next server. -/
def dnsSentAt : Nat → Nat
  | 0 => 0
  | 1 => 1
  | 2 => 2
  | 3 => 4
  | n + 4 => 7 + dnsSentAt n

/-- Four queries go out in the 5 s, all to the first resolver. The second
resolver's turn comes at 7 s, after the wait is over. -/
theorem dns_tries : (List.range 5).map dnsSentAt = [0, 1, 2, 4, 7] ∧ 5 < dnsSentAt 4 := by decide

/-- lwIP repeats a SYN every 3 s and does not lengthen the wait. -/
def lwipSynAt (n : Nat) : Nat := 3 * n
/-- RFC 6298: the first wait is 1 s (§2.1) and each later one doubles (§5.5). -/
def rfcSynAt (n : Nat) : Nat := 2 ^ n - 1

/-- In the 8 s an app waits, lwIP sends three SYNs and the RFC's timing four. -/
theorem syn_tries (n : Nat) (h : n < 8) :
    (lwipSynAt n < 8 ↔ n < 3) ∧ (rfcSynAt n < 8 ↔ n < 4) := by
  unfold lwipSynAt rfcSynAt
  have : n = 0 ∨ n = 1 ∨ n = 2 ∨ n = 3 ∨ n = 4 ∨ n = 5 ∨ n = 6 ∨ n = 7 := by omega
  rcases this with h | h | h | h | h | h | h | h <;> subst h <;> decide

/-! ## 6. The station address (wlan_axi.rs, radio_io.rs; RFC 9542 §2.1)

The first byte's low bit marks a group address and the next bit a locally
assigned one. -/

def unicast (first : Nat) : Bool := first % 2 = 0
def localAddress (first : Nat) : Bool := first / 2 % 2 = 1

/-- The random address the phone makes for a session, `(b & !3) | 2`, is
always a unicast, locally assigned one. -/
theorem random_address_is_local (b : Nat) :
    unicast (b / 4 * 4 + 2) = true ∧ localAddress (b / 4 * 4 + 2) = true := by
  unfold unicast localAddress; simp; omega

/-- `fixed_address` takes any unicast address. One whose first byte is 04 is
accepted, yet it lies in the space the IEEE assigns to manufacturers. -/
theorem fixed_address_need_not_be_local : unicast 0x04 = true ∧ localAddress 0x04 = false := by
  decide

/-! ## 7. IP over IEEE 802 (station.rs `ethernet`; RFC 1042)

An IP or ARP packet rides in an 802.11 data frame behind the LLC and SNAP
header AA AA 03 00 00 00 and its EtherType. -/

def snap : List Nat := [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00]

def encapsulate (etherType : Nat × Nat) (payload : List Nat) : List Nat :=
  snap ++ [etherType.1, etherType.2] ++ payload

def decapsulate : List Nat → Option ((Nat × Nat) × List Nat)
  | 0xAA :: 0xAA :: 0x03 :: 0x00 :: 0x00 :: 0x00 :: hi :: lo :: payload => some ((hi, lo), payload)
  | _ => none

/-- What was wrapped comes back unwrapped. -/
theorem snap_round_trip (etherType : Nat × Nat) (payload : List Nat) :
    decapsulate (encapsulate etherType payload) = some (etherType, payload) := by
  simp [encapsulate, snap, decapsulate]

/-- A frame without the header is not taken for IP. -/
theorem snap_required (first : Nat) (rest : List Nat) (h : first ≠ 0xAA) :
    decapsulate (first :: rest) = none := by
  unfold decapsulate; split <;> simp_all

end WifiStack
