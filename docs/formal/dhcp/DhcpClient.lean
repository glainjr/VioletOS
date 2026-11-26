/-
The DHCP client of RFC 2131 (docs/references/rfc2131.txt) as the Moto IP client
runs it: targets/moto-g-pure/ip/client.c over lwIP 2.2.1. Three parts:

  1. the client's states, and that an address is used only under a live lease
  2. the renewal timers lwIP derives from a lease
  3. when lwIP repeats DHCPDISCOVER, against the 30 s a join waits

Check with `lean DhcpClient.lean` in this directory: no output means every
theorem holds. These are proofs about this model, not about the C code;
targets/moto-g-pure/ip/test_dhcp.c holds the code to the same states and rules.
-/
namespace Dhcp

/-! ## 1. States (RFC 2131 §4.4, Figure 5)

INIT is folded into `selecting`: lwIP sends DHCPDISCOVER in the same call that
enters INIT. INIT-REBOOT and REBOOTING are left out: the phone never asks for a
remembered address. `checking` is lwIP's state for the address check of RFC 2131
§3.1 step 5; the phone has that check turned off (ip/lwipopts.h). -/

inductive Phase where
  | off | selecting | requesting | checking | bound | renewing | rebinding
  deriving DecidableEq, Repr

/-- Lease time at which the client starts renewing: T1 (RFC 2131 §4.4.5). -/
def renewAt (lease : Nat) : Nat := lease / 2
/-- Lease time at which the client asks any server: T2 (RFC 2131 §4.4.5). -/
def rebindAt (lease : Nat) : Nat := lease * 7 / 8

/-- RFC 2131 §4.4.5: T1 MUST be earlier than T2, and T2 earlier than the lease's end. -/
theorem renew_before_rebind (lease : Nat) (h : 8 ≤ lease) :
    renewAt lease < rebindAt lease ∧ rebindAt lease < lease := by
  unfold renewAt rebindAt; omega

structure State where
  phase : Phase
  /-- Duration of the lease in use. -/
  lease : Nat
  /-- Time since that lease was granted. -/
  age : Nat
  /-- Duration offered by the DHCPACK whose address is being checked. -/
  pending : Nat
  deriving Repr

def State.start : State := ⟨.off, 0, 0, 0⟩
def State.discover : State := ⟨.selecting, 0, 0, 0⟩

/-- The client is sending and receiving with a DHCP address. -/
def State.usesAddress (s : State) : Prop :=
  s.phase = .bound ∨ s.phase = .renewing ∨ s.phase = .rebinding

/-- What can happen to the client. `current` says whether a server message
carries the transaction id ('xid') of the request the client has outstanding
(RFC 2131 §4.1); every new exchange picks a new id. -/
inductive Event where
  /-- `station_net_join` brings the interface up. -/
  | join
  | offer (current : Bool)
  | ack (current : Bool) (lease : Nat)
  | nak (current : Bool)
  /-- No answer to DHCPREQUEST after retrying (RFC 2131 §3.1 step 5). -/
  | timeout
  /-- The address check ended, with or without an answer for the address. -/
  | checked (inUse : Bool)
  /-- One unit of lease time passes. -/
  | tick
  /-- `station_net_close`, which a failed join also calls. -/
  | close

/-- One step of the client. `check` is whether the address check is on. -/
def step (check : Bool) (s : State) : Event → State
  | .join => State.discover
  | .close => State.start
  | .offer true => if s.phase = .selecting then { s with phase := .requesting } else s
  | .ack true lease =>
    if lease = 0 then s
    else if s.phase = .requesting then
      if check then { s with phase := .checking, pending := lease }
      else ⟨.bound, lease, 0, 0⟩
    else if s.phase = .renewing ∨ s.phase = .rebinding then ⟨.bound, lease, 0, 0⟩
    else s
  | .nak true =>
    if s.phase = .requesting ∨ s.phase = .renewing ∨ s.phase = .rebinding then State.discover
    else s
  | .timeout => if s.phase = .requesting then State.discover else s
  | .checked inUse =>
    if s.phase = .checking then
      if inUse then State.discover else ⟨.bound, s.pending, 0, 0⟩
    else s
  | .tick =>
    if s.phase = .bound ∨ s.phase = .renewing ∨ s.phase = .rebinding then
      if s.lease ≤ s.age + 1 then State.discover
      else if rebindAt s.lease ≤ s.age + 1 then { s with phase := .rebinding, age := s.age + 1 }
      else if renewAt s.lease ≤ s.age + 1 then { s with phase := .renewing, age := s.age + 1 }
      else { s with age := s.age + 1 }
    else s
  -- A message for an exchange the client has since abandoned is discarded.
  | .offer false | .ack false _ | .nak false => s

def run (check : Bool) (events : List Event) : State :=
  events.foldl (step check) State.start

/-- What holds of every state the client can reach. -/
structure Sound (s : State) : Prop where
  /-- RFC 2131 §4.4.5: the address is not used once its lease has run out. -/
  live : s.usesAddress → s.age < s.lease
  /-- The client asks to renew no earlier than T1 and to rebind no earlier than T2. -/
  renewing : s.phase = .renewing → renewAt s.lease ≤ s.age
  rebinding : s.phase = .rebinding → rebindAt s.lease ≤ s.age
  /-- An address under check came with a lease. -/
  pending : s.phase = .checking → 0 < s.pending

theorem sound_start : Sound State.start := by
  constructor <;> simp [State.start, State.usesAddress]

theorem sound_discover : Sound State.discover := by
  constructor <;> simp [State.discover, State.usesAddress]

theorem sound_bound (lease : Nat) (h : 0 < lease) : Sound ⟨.bound, lease, 0, 0⟩ := by
  constructor <;> simp [State.usesAddress] <;> omega

theorem sound_step (check : Bool) (s : State) (e : Event) (h : Sound s) :
    Sound (step check s e) := by
  cases e with
  | join => exact sound_discover
  | close => exact sound_start
  | offer current =>
    cases current
    · exact h
    · simp only [step]; split
      · rename_i hp
        constructor <;> simp [State.usesAddress]
      · exact h
  | ack current lease =>
    cases current
    · exact h
    · simp only [step]
      split
      · exact h
      · rename_i hl
        split
        · cases check
          · exact sound_bound lease (by omega)
          · constructor <;> simp [State.usesAddress]; omega
        · split
          · exact sound_bound lease (by omega)
          · exact h
  | nak current =>
    cases current
    · exact h
    · simp only [step]; split
      · exact sound_discover
      · exact h
  | timeout =>
    simp only [step]; split
    · exact sound_discover
    · exact h
  | checked inUse =>
    simp only [step]; split
    · rename_i hp
      cases inUse
      · exact sound_bound s.pending (h.pending hp)
      · exact sound_discover
    · exact h
  | tick =>
    simp only [step]; split
    · rename_i hu
      split
      · exact sound_discover
      · split
        · constructor <;> simp [State.usesAddress] <;> omega
        · split
          · constructor <;> simp [State.usesAddress] <;> omega
          · rename_i h1 h2 h3
            have hr := h.renewing
            have hb := h.rebinding
            constructor
            · intro _; simp; omega
            · intro hp; simp at hp ⊢; have := hr hp; omega
            · intro hp; simp at hp ⊢; have := hb hp; omega
            · intro hp; simp at hp; rcases hu with hu | hu | hu <;> simp [hu] at hp
    · exact h

/-- Whatever happens, in whatever order, the client's state stays sound. -/
theorem sound_run (check : Bool) (events : List Event) : Sound (run check events) := by
  suffices ∀ s, Sound s → Sound (events.foldl (step check) s) from this _ sound_start
  induction events with
  | nil => intro s h; exact h
  | cons e rest ih => intro s h; exact ih _ (sound_step check s e h)

/-- A DHCPACK the client can act on: for its outstanding request, with a lease. -/
def Event.grants : Event → Bool
  | .ack true lease => lease != 0
  | _ => false

/-- No address without a server granting it to this exchange (RFC 2131 §4.1, §4.4.1):
offers, refusals and acknowledgements of abandoned exchanges never yield one. -/
theorem no_address_without_grant (check : Bool) (events : List Event)
    (h : ∀ e ∈ events, e.grants = false) :
    ¬ (run check events).usesAddress ∧ (run check events).phase ≠ .checking := by
  suffices ∀ s, (¬ s.usesAddress ∧ s.phase ≠ .checking) →
      (∀ e ∈ events, e.grants = false) →
      ¬ (events.foldl (step check) s).usesAddress ∧ (events.foldl (step check) s).phase ≠ .checking from
    this _ (by simp [State.start, State.usesAddress]) h
  clear h
  induction events with
  | nil => intro s hs _; exact hs
  | cons e rest ih =>
    intro s hs hall
    apply ih
    · have he := hall e (by simp)
      obtain ⟨hu, hc⟩ := hs
      simp only [State.usesAddress, not_or] at hu
      obtain ⟨h1, h2, h3⟩ := hu
      cases e with
      | join => simp [step, State.discover, State.usesAddress]
      | close => simp [step, State.start, State.usesAddress]
      | offer current =>
        cases current <;> simp only [step]
        · simp [State.usesAddress, *]
        · split <;> simp [State.usesAddress, *]
      | ack current lease =>
        cases current
        · simp [step, State.usesAddress, *]
        · have : lease = 0 := by simpa [Event.grants] using he
          simp [step, State.usesAddress, *]
      | nak current =>
        cases current <;> simp only [step]
        · simp [State.usesAddress, *]
        · split <;> simp [State.discover, State.usesAddress, *]
      | timeout => simp only [step]; split <;> simp [State.discover, State.usesAddress, *]
      | checked inUse => simp [step, State.usesAddress, *]
      | tick => simp [step, State.usesAddress, *]
    · intro e' he'; exact hall e' (by simp [he'])

/-- With the address check on, an answer for the address every time (an access
point answering ARP on the phone's behalf, RFC 5227 §2.1.1) means no address,
however often the server grants one. -/
theorem always_in_use_never_bound (events : List Event)
    (h : ∀ e ∈ events, e ≠ .checked false) :
    ¬ (run true events).usesAddress := by
  suffices ∀ s, ¬ s.usesAddress → (∀ e ∈ events, e ≠ .checked false) →
      ¬ (events.foldl (step true) s).usesAddress from
    this _ (by simp [State.start, State.usesAddress]) h
  clear h
  induction events with
  | nil => intro s hs _; exact hs
  | cons e rest ih =>
    intro s hs hall
    apply ih
    · have he := hall e (by simp)
      simp only [State.usesAddress, not_or] at hs
      obtain ⟨h1, h2, h3⟩ := hs
      cases e with
      | join => simp [step, State.discover, State.usesAddress]
      | close => simp [step, State.start, State.usesAddress]
      | offer current =>
        cases current <;> simp only [step]
        · simp [State.usesAddress, *]
        · split <;> simp [State.usesAddress, *]
      | ack current lease =>
        cases current <;> simp only [step]
        · simp [State.usesAddress, *]
        · split
          · simp [State.usesAddress, *]
          · split <;> simp [State.usesAddress, *]
      | nak current =>
        cases current <;> simp only [step]
        · simp [State.usesAddress, *]
        · split <;> simp [State.discover, State.usesAddress, *]
      | timeout => simp only [step]; split <;> simp [State.discover, State.usesAddress, *]
      | checked inUse =>
        cases inUse
        · exact absurd rfl he
        · simp only [step]; split <;> simp [State.discover, State.usesAddress, *]
      | tick => simp [step, State.usesAddress, *]
    · intro e' he'; exact hall e' (by simp [he'])

/-- With the check off, the first grant is used at once: nothing on the network
can come between the DHCPACK and the address. -/
theorem grant_binds_at_once (s : State) (lease : Nat) (hp : s.phase = .requesting)
    (hl : 0 < lease) : (step false s (.ack true lease)).phase = .bound := by
  have : lease ≠ 0 := by omega
  simp [step, hp, this]

/-! ## 2. The timers lwIP derives from a lease

lwIP counts lease time in minutes (DHCP_COARSE_TIMER_SECS, 60) in 16-bit
counters: SET_TIMEOUT_FROM_OFFERED in core/ipv4/dhcp.c. Each minute it tests
for the lease's end first, then T2, then T1 (dhcp_coarse_tmr). -/

/-- Seconds to minutes, rounded, at least 1 and at most 65535. -/
def ticks (seconds : Nat) : Nat :=
  if (seconds + 30) / 60 = 0 then 1
  else if 65535 < (seconds + 30) / 60 then 65535
  else (seconds + 30) / 60

/-- The minute the lease ends, and the minutes T1 and T2 fire, when the server
names no T1 or T2 (dhcp_handle_ack: lease / 2 and lease * 7 / 8). -/
def endTick (lease : Nat) : Nat := ticks lease
def renewTick (lease : Nat) : Nat := ticks (lease / 2)
def rebindTick (lease : Nat) : Nat := ticks (lease * 7 / 8)

/-- From 8 minutes to 45 days, lwIP's timers keep the order RFC 2131 §4.4.5 requires. -/
theorem timers_ordered (lease : Nat) (lo : 480 ≤ lease) (hi : lease ≤ 3932129) :
    renewTick lease < rebindTick lease ∧ rebindTick lease < endTick lease := by
  unfold renewTick rebindTick endTick ticks
  repeat' split
  all_goals omega

/-- A lease under 90 s ends at its first minute with no attempt to renew it:
the client drops the address and starts again every minute. -/
theorem short_lease_not_renewed (lease : Nat) (h : lease < 90) :
    renewTick lease = endTick lease ∧ rebindTick lease = endTick lease := by
  unfold renewTick rebindTick endTick ticks
  repeat' split
  all_goals omega

/-- A lease of 91 days or more is never renewed either: T1, T2 and the end all
land on minute 65535, and the end is tested first. The address is dropped after
45.5 days. -/
theorem long_lease_not_renewed (lease : Nat) (h : 7864140 ≤ lease) :
    renewTick lease = 65535 ∧ rebindTick lease = 65535 ∧ endTick lease = 65535 := by
  unfold renewTick rebindTick endTick ticks
  repeat' split
  all_goals omega

/-- `lease * 7` is computed in 32 bits; it cannot overflow below 19 years. -/
theorem rebind_fits_32_bits (lease : Nat) (h : lease ≤ 613566756) : lease * 7 < 2 ^ 32 := by
  omega

/-! ## 3. Repeating DHCPDISCOVER (RFC 2131 §4.1)

RFC 2131 §4.1 asks for 4 s before the first repeat, doubling to at most 64 s,
each randomised by up to a second. lwIP waits 2 s, doubling to 32 s and then
60 s, with no randomness (dhcp_discover). `station_net_join` waits 30 s. -/

/-- Seconds lwIP waits after its n-th DHCPDISCOVER, n from 1. -/
def lwipWait (n : Nat) : Nat := if n < 6 then 2 ^ n else 60
/-- Seconds RFC 2131 §4.1 suggests after the n-th, before randomising. -/
def rfcWait (n : Nat) : Nat := if n < 5 then 2 ^ (n + 1) else 64

/-- When the n-th DHCPDISCOVER leaves, in seconds from the first; n from 0. -/
def sentAt : Nat → Nat
  | 0 => 0
  | n + 1 => sentAt n + lwipWait (n + 1)

/-- How long a join waits for an address: `station_net_join` in ip/client.c. -/
def joinWait : Nat := 30

theorem first_discovers : (List.range 6).map sentAt = [0, 2, 6, 14, 30, 62] := by decide

/-- lwIP never waits longer between tries than RFC 2131 suggests. -/
theorem lwip_no_slower (n : Nat) (h : 1 ≤ n) : lwipWait n ≤ rfcWait n := by
  unfold lwipWait rfcWait
  match n, h with
  | 1, _ | 2, _ | 3, _ | 4, _ | 5, _ => decide
  | n + 6, _ => simp [show ¬ (n + 6 < 6) by omega, show ¬ (n + 6 < 5) by omega]

theorem sentAt_mono (n : Nat) : sentAt n ≤ sentAt (n + 1) := by
  simp [sentAt]

theorem late_discovers (n : Nat) : joinWait ≤ sentAt (n + 4) := by
  induction n with
  | zero => decide
  | succ k ih => exact Nat.le_trans ih (sentAt_mono (k + 4))

/-- Only four requests leave before the join gives up, the last at 14 s. A
server that hears none of them cannot be joined. -/
theorem four_tries (n : Nat) : sentAt n < joinWait ↔ n < 4 := by
  constructor
  · intro h
    if hn : n < 4 then exact hn
    else
      have := late_discovers (n - 4)
      have e : n - 4 + 4 = n := by omega
      rw [e] at this; omega
  · intro h
    match n, h with
    | 0, _ | 1, _ | 2, _ | 3, _ => decide

end Dhcp
