//! Wi-Fi for open apps: the "wifi" commands in /dev/violet/sys.
//!
//!   wifi                    state=idle|scanning|joining|leaving connected=ID
//!                           ssid=HEX hosting=0|1 last=scan|join|leave|-
//!                           result=ok|failed|- reason=N
//!                           (hosting: Radio hosts Phone Internet, not a network)
//!   wifi scan               start a scan (answers at once; poll "wifi")
//!   wifi list               one line per network from the last scan:
//!                           id=ID channel=N security=wpa2|open|other ssid=HEX
//!   wifi join ID [PMK]      join a network; PMK is 64 hex digits for WPA2
//!   wifi leave              leave the joined network
//!
//! While Settings › Radios has Wi-Fi off (radios.rs), scan, join and
//! "relay on" answer "error off", the state says radio=off, and neither the
//! auto-connector nor Phone Internet's restore runs.
//!
//! Saved networks rejoin on their own: the Wi-Fi app keeps them in
//! /data/networks (most recently joined first), and the auto-connector here
//! scans and joins the first one in range a few seconds after boot, and again
//! with a growing wait (30 s up to 10 min) while none is in range or the link
//! is gone. A leave from the Wi-Fi app pauses it until the app joins again.
//! It never changes the saved keys: only the Wi-Fi app does.
//!
//! Radio's requests can take seconds on the handset (it boots the Wi-Fi chip,
//! scans and runs the WPA2 handshake), so they run as Runtime tickets polled
//! from here, and other apps keep running meanwhile. Only /apps/wifi may join
//! or leave a network: the others can look but not change it.
use super::{fs::Fs, request, sys};
use crate::log;
use crate::protocol::*;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The program allowed to change the network.
pub const SETTINGS_APP: &str = "/apps/settings";

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Scan,
    Join,
    Leave,
    /// The auto-connector asks which network is joined (GET PROFILE), as a
    /// ticket like the others: Radio may be busy rejoining for many seconds.
    Check,
    /// Phone Internet (RELAY): turn it on or off, or ask how it is.
    Relay,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::Scan => "scan",
            Step::Join => "join",
            Step::Leave => "leave",
            Step::Check => "check",
            Step::Relay => "relay",
        }
    }
}

#[derive(Default)]
pub struct Wifi {
    /// The request Runtime is working on: what it is, and its ticket.
    job: Option<(Step, u32)>,
    /// Requests still to send once it finishes (leave, scan, then join).
    queue: VecDeque<(Step, Message)>,
    /// The outcome of the last request the app asked for: (what, ok, reason).
    last: Option<(Step, bool, u32)>,
    connected: u32,
    /// The joined network's SSID, by profile ID.
    name: (u32, Vec<u8>),
    /// The running chain was started by the auto-connector, not by an app.
    auto_job: bool,
    auto: Auto,
    /// Phone Internet: Radio's last answer (op, state, failure code), and
    /// whether the saved choice was restored after boot.
    relay: Option<(u32, u32, u32)>,
    relay_restored: bool,
    /// Settings › Radios turned Wi-Fi off (radios.rs).
    off: bool,
}

const OFF: &str = "error off: Wi-Fi is turned off in Settings › Radios\n";

/// The Phone Internet pairing Settings keeps: "on CODE" or "off CODE".
pub const RELAY_SAVED: &str = "/data/relay";
/// When after boot to turn a saved Phone Internet back on (before the
/// auto-connector looks, so it wins over a saved network).
const RELAY_RESTORE_NS: u64 = 3_000_000_000;

/// Where the auto-connector is (see the module comment).
#[derive(Default)]
struct Auto {
    /// When it looks next (ns since boot); 0 until the first tick.
    next_ns: u64,
    /// Failed attempts in a row, for the growing wait.
    tries: u32,
    /// The Wi-Fi app left the network on purpose: wait for it to join again.
    paused: bool,
    /// Saved networks (SSID in hex) that failed since the last scan round.
    /// A refused handshake (JOIN_AUTH) is retried on later rounds too: an access
    /// point also repeats its first message when our answer was lost, so one 52
    /// does not prove the key is wrong (seen on the handset after a reboot).
    skip: Vec<String>,
    /// The network a running auto-join is for (its SSID in hex).
    target: String,
    /// An auto scan finished: choose a network on the next tick.
    scanned: bool,
}

/// How many scan results Radio reports (targets/moto-g-pure/passive_scan.rs).
const SCAN_ROWS: u32 = 32;

/// Saved networks, as the Wi-Fi app writes them: one per line, "SSID KEY" in
/// hex, or "SSID open". Most recently joined first.
pub const SAVED: &str = "/data/networks";
/// How long after boot before the first look: Home turns Phone Internet back
/// on in its first seconds, and that wins over a saved network.
const FIRST_LOOK_NS: u64 = 8_000_000_000;
/// While joined, how often to check the link is still there.
const WATCH_NS: u64 = 30_000_000_000;

/// The wait after `tries` failed attempts in a row: 30 s, 60 s, ... up to 10 min.
fn backoff_ns(tries: u32) -> u64 {
    (30_000_000_000u64 << tries.saturating_sub(1).min(5)).min(600_000_000_000)
}

/// The profile ID Radio reports while it hosts Phone Internet (targets/moto-g-pure/radio.rs).
const HOSTING: u32 = 0x5245_4c59;

fn submit(m: Message) -> Result<u32, u32> {
    let r = m.call(sys::SLOT_RUNTIME);
    if r.op == OK {
        Ok(r.value)
    } else {
        Err(r.op)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(key)
}

impl Wifi {
    /// Advance the running request and start the next one.
    fn poll(&mut self) {
        loop {
            if let Some((step, ticket)) = self.job {
                let p = Message::new(POLL, 0, ticket).call(sys::SLOT_RUNTIME);
                if p.op == BUSY {
                    return;
                }
                self.job = None;
                if step == Step::Relay {
                    self.relay = Some((p.op, p.value, p.arg));
                    continue;
                }
                let ok = p.op == OK;
                match step {
                    Step::Join | Step::Check if ok => self.connected = p.value,
                    Step::Leave if ok => self.connected = 0,
                    _ => {}
                }
                // A failed step ends the chain: a join after a failed leave would be refused.
                if !ok {
                    self.queue.clear();
                }
                if self.queue.is_empty() {
                    if self.auto_job {
                        self.auto_job = false;
                        self.auto_done(step, ok, if ok { 0 } else { p.value });
                    } else {
                        self.last = Some((step, ok, if ok { 0 } else { p.value }));
                    }
                }
            }
            let Some((step, m)) = self.queue.pop_front() else { return };
            match submit(m) {
                Ok(ticket) => self.job = Some((step, ticket)),
                Err(code) => {
                    self.queue.clear();
                    if core::mem::take(&mut self.auto_job) {
                        self.auto_done(step, false, code);
                    } else {
                        self.last = Some((step, false, code));
                    }
                    return;
                }
            }
        }
    }

    fn start(&mut self, steps: &[(Step, Message)]) -> String {
        self.poll();
        if self.job.is_some() {
            return String::from("error busy\n");
        }
        self.last = None;
        self.auto_job = false;
        self.auto.scanned = false;
        self.queue.extend(steps.iter().copied());
        self.poll();
        String::from("ok\n")
    }

    /// Settings › Radios turns Wi-Fi off or on. Off drops whatever was waiting
    /// and asks Radio to leave, which ends Phone Internet too and powers the
    /// connectivity chip down; on lets the auto-connector look again.
    pub fn set_off(&mut self, off: bool) {
        if off == self.off {
            return;
        }
        self.off = off;
        if off {
            // Radio answers OFFLINE when nothing runs; as the auto-connector's
            // own request, that is not reported to the Wi-Fi app as a failed leave.
            self.queue.clear();
            self.auto_job = true;
            self.queue.push_back((Step::Leave, Message::new(EXECUTE, PROFILE, 0)));
            self.relay = None;
            self.poll();
        } else {
            self.auto.tries = 0;
            self.auto.skip.clear();
            self.auto.next_ns = 1;
        }
    }

    /// The auto-connector's turn, from the runtime's poll (every 50 ms or so).
    pub fn tick(&mut self, fs: &Fs, now: u64) {
        self.poll();
        if self.off {
            return;
        }
        if self.auto.next_ns == 0 {
            self.auto.next_ns = now + FIRST_LOOK_NS;
        }
        if !self.relay_restored && now >= RELAY_RESTORE_NS && self.job.is_none() {
            self.relay_restored = true;
            let saved = core::str::from_utf8(fs.contents(RELAY_SAVED).unwrap_or(&[])).unwrap_or("");
            let mut w = saved.split_whitespace();
            if let (Some("on"), Some(code)) = (w.next(), w.next()) {
                if valid_relay_code(code.as_bytes()) {
                    log!("phone internet: turning it back on");
                    let mut m = Message::new(EXECUTE, RELAY, 1);
                    m.put_text(code.as_bytes());
                    self.queue.push_back((Step::Relay, m));
                    self.poll();
                }
            }
        }
        if self.job.is_some() || self.auto.paused || now < self.auto.next_ns {
            return;
        }
        let saved = core::str::from_utf8(fs.contents(SAVED).unwrap_or(&[])).unwrap_or("");
        if core::mem::take(&mut self.auto.scanned) {
            self.auto_join(saved, now);
        } else if saved.trim().is_empty() {
            self.auto.next_ns = now + WATCH_NS;
        } else {
            self.auto_start(Step::Check, Message::new(GET, PROFILE, 0));
        }
        self.poll();
    }

    /// Queue an auto-connector request; poll() sends it.
    fn auto_start(&mut self, step: Step, m: Message) {
        self.auto_job = true;
        self.auto.next_ns = u64::MAX; // auto_done sets the next look
        self.queue.push_back((step, m));
    }

    /// After an auto scan: join the first saved network in range. `saved` is
    /// the Wi-Fi app's file, most recently joined first: "SSID KEY" in hex, or
    /// "SSID open".
    fn auto_join(&mut self, saved: &str, now: u64) {
        let pick = saved.lines().find_map(|line| {
            let mut w = line.split_whitespace();
            let (ssid, key) = (w.next()?, w.next()?);
            let key = if key == "open" { None } else { Some(unhex(key)?) };
            let flag = if key.is_some() { SCAN_WPA2 } else { SCAN_OPEN };
            if self.auto.skip.iter().any(|r| r == ssid) {
                return None;
            }
            (0..SCAN_ROWS)
                .map(|i| request(GET, SCAN, i))
                .take_while(|r| r.op == OK && r.value != 0)
                .find(|r| {
                    let text = r.text();
                    let n = text.iter().position(|&b| b == 0).unwrap_or(text.len()).min(32);
                    r.arg & flag != 0 && hex(&text[..n]) == ssid
                })
                .map(|r| (r.value, ssid, key))
        });
        let Some((id, ssid, key)) = pick else {
            self.auto.skip.clear();
            self.auto.tries += 1;
            self.auto.next_ns = now + backoff_ns(self.auto.tries);
            return;
        };
        let mut join = Message::new(EXECUTE, PROFILE, id);
        join.data = key.unwrap_or([0; 32]);
        self.auto.target = String::from(ssid);
        self.auto_start(Step::Join, join);
        join.data = [0; 32];
    }

    /// An auto-connector request finished.
    fn auto_done(&mut self, step: Step, ok: bool, reason: u32) {
        let now = sys::now_ns();
        let name = core::mem::take(&mut self.auto.target);
        if step == Step::Join {
            log!("wifi: auto-join {} result {}", name, reason);
        }
        if step == Step::Check {
            if ok && self.connected == 0 {
                // Nothing joined: scan first, as the handset joins only what its
                // last scan found.
                return self.auto_start(Step::Scan, Message::new(EXECUTE, RADIO, 1));
            }
            // Joined (or hosting Phone Internet, or Radio is busy): look again later.
            self.auto.next_ns = now + WATCH_NS;
            return;
        }
        if ok {
            // A scan: choose a network on the next tick. A join: watch the link.
            self.auto.scanned = step == Step::Scan;
            if step != Step::Scan {
                self.auto.tries = 0;
            }
            self.auto.next_ns = if step == Step::Scan { now } else { now + WATCH_NS };
            return;
        }
        if step == Step::Join {
            self.auto.skip.push(name);
            // Try the next saved network the last scan found.
            self.auto.scanned = true;
            self.auto.next_ns = now;
            return;
        }
        self.auto.tries += 1;
        self.auto.next_ns = now + backoff_ns(self.auto.tries);
    }

    /// Answer one "wifi ..." command from `path`.
    pub fn command(&mut self, path: &str, words: &mut core::str::SplitWhitespace) -> String {
        let sub = words.next();
        if matches!(sub, Some("join" | "leave")) && path != SETTINGS_APP {
            return String::from("error denied: only Settings changes the network\n");
        }
        match sub {
            None | Some("state") => {
                self.poll();
                if self.job.is_none() {
                    let p = request(GET, PROFILE, 0);
                    if p.op == OK {
                        self.connected = p.value;
                    }
                }
                let state = match self.job.map(|j| j.0) {
                    None | Some(Step::Check) => "idle",
                    Some(Step::Scan) => "scanning",
                    Some(Step::Join) => "joining",
                    Some(Step::Leave) => "leaving",
                    Some(Step::Relay) => "idle",
                };
                let (last, result, reason) = match self.last {
                    None => ("-", "-", 0),
                    Some((s, ok, r)) => (s.name(), if ok { "ok" } else { "failed" }, r),
                };
                if self.job.is_none() && self.connected != 0 && self.connected != HOSTING && self.name.0 != self.connected {
                    self.name = (self.connected, Vec::new());
                    if let Some(n) = (0..SCAN_ROWS).map(|i| request(GET, SCAN, i)).take_while(|r| r.op == OK && r.value != 0).find(|r| r.value == self.connected) {
                        let text = n.text();
                        let len = text.iter().position(|&b| b == 0).unwrap_or(text.len()).min(32);
                        self.name.1 = text[..len].to_vec();
                    }
                }
                let ssid = if self.connected != 0 && self.name.0 == self.connected { hex(&self.name.1) } else { String::new() };
                format!(
                    "state={} connected={} ssid={} hosting={} last={} result={} reason={} radio={}\n",
                    state, self.connected, ssid, (self.connected == HOSTING) as u32, last, result, reason,
                    if self.off { "off" } else { "on" }
                )
            }
            Some("scan" | "join") if self.off => String::from(OFF),
            Some("scan") => self.start(&[(Step::Scan, Message::new(EXECUTE, RADIO, 1))]),
            Some("list") => {
                self.poll();
                if self.job.is_some() {
                    return String::from("error busy\n");
                }
                let mut out = String::new();
                for i in 0..SCAN_ROWS {
                    let r = request(GET, SCAN, i);
                    if r.op != OK || r.value == 0 {
                        break;
                    }
                    let text = r.text();
                    let n = text.iter().position(|&b| b == 0).unwrap_or(text.len()).min(32);
                    let security = if r.arg & SCAN_WPA2 != 0 {
                        "wpa2"
                    } else if r.arg & SCAN_OPEN != 0 {
                        "open"
                    } else {
                        "other"
                    };
                    out += &format!("id={} channel={} security={} ssid={}\n", r.value, r.arg & 0xff, security, hex(&text[..n]));
                }
                if out.is_empty() {
                    out.push('\n');
                }
                out
            }
            Some("join") => {
                let Some(id) = words.next().and_then(|w| w.parse::<u32>().ok()).filter(|&id| id != 0) else {
                    return String::from("error usage: wifi join ID [PMK]\n");
                };
                let mut join = Message::new(EXECUTE, PROFILE, id);
                if let Some(k) = words.next() {
                    match unhex(k) {
                        Some(key) => join.data = key,
                        None => return String::from("error the key is 64 hex digits\n"),
                    }
                }
                self.poll();
                if self.job.is_none() {
                    let p = request(GET, PROFILE, 0);
                    if p.op == OK {
                        self.connected = p.value;
                    }
                }
                // Radio joins one network at a time: leave the current one, scan
                // again (the handset needs a fresh scan to join), then join.
                let answer = if self.connected != 0 && self.connected != id {
                    self.start(&[
                        (Step::Leave, Message::new(EXECUTE, PROFILE, 0)),
                        (Step::Scan, Message::new(EXECUTE, RADIO, 1)),
                        (Step::Join, join),
                    ])
                } else {
                    self.start(&[(Step::Join, join)])
                };
                join.data = [0; 32];
                if answer == "ok\n" {
                    // The app chose a network: rejoin on its own again from now on.
                    self.auto.paused = false;
                    self.auto.skip.clear();
                    self.auto.tries = 0;
                }
                answer
            }
            Some("leave") => {
                let answer = self.start(&[(Step::Leave, Message::new(EXECUTE, PROFILE, 0))]);
                if answer == "ok\n" {
                    self.auto.paused = true;
                }
                answer
            }
            Some(_) => String::from("error usage: wifi [state|scan|list|join ID [PMK]|leave]\n"),
        }
    }

    /// Phone Internet ("relay" in /dev/violet/sys): the iPhone running Violet
    /// Relay joins a network this phone hosts and lends it cellular data.
    ///
    ///   relay              state=off|starting|waiting|joined|online|unavailable|busy failure=N
    ///   relay on CODE      host the network for a 12-character pairing code
    ///   relay off          stop hosting
    ///
    /// Only Settings may turn it on or off; Settings saves the choice in
    /// /data/relay, and the runtime turns it back on after a restart.
    pub fn relay(&mut self, path: &str, words: &mut core::str::SplitWhitespace) -> String {
        let sub = words.next();
        if sub.is_some() && path != SETTINGS_APP {
            return String::from("error denied: only Settings changes Phone Internet\n");
        }
        self.poll();
        match sub {
            None => {
                let busy = self.job.is_some_and(|j| j.0 == Step::Relay);
                if self.job.is_none() {
                    self.queue.push_back((Step::Relay, Message::new(GET, RELAY, 0)));
                    self.poll();
                }
                let state = match self.relay {
                    _ if busy => "busy",
                    None => "busy",
                    Some((op, _, _)) if op != OK => "unavailable",
                    Some((_, RELAY_STARTING, _)) => "starting",
                    Some((_, RELAY_WAITING, _)) => "waiting",
                    Some((_, RELAY_JOINED, _)) => "joined",
                    Some((_, RELAY_ONLINE, _)) => "online",
                    Some(_) => "off",
                };
                format!("state={} failure={}\n", state, self.relay.map_or(0, |r| r.2))
            }
            Some("on") if self.off => String::from(OFF),
            Some("on") => {
                let Some(code) = words.next().filter(|c| valid_relay_code(c.as_bytes())) else {
                    return String::from("error usage: relay on CODE (12 of 0-9 A-F)\n");
                };
                if self.job.is_some() {
                    return String::from("error busy\n");
                }
                let mut m = Message::new(EXECUTE, RELAY, 1);
                m.put_text(code.as_bytes());
                self.relay = Some((OK, RELAY_STARTING, 0));
                self.queue.push_back((Step::Relay, m));
                self.poll();
                String::from("ok\n")
            }
            Some("off") => {
                if self.job.is_some() {
                    return String::from("error busy\n");
                }
                self.queue.push_back((Step::Relay, Message::new(EXECUTE, RELAY, 0)));
                self.poll();
                String::from("ok\n")
            }
            Some(_) => String::from("error usage: relay [on CODE|off]\n"),
        }
    }
}
