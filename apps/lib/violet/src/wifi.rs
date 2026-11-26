//! Wi-Fi through the "wifi" commands in /dev/violet/sys (runtime/open/wifi.rs).
//! Anyone may scan and read the state; only /apps/wifi may join or leave.
use std::num::NonZeroU32;

use crate::device::{fields, sys};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Security {
    /// WPA2-Personal: needs a password.
    Wpa2,
    Open,
    /// Something Radio cannot join yet (WPA3, Enterprise, WEP).
    Other,
}

#[derive(Clone, Debug)]
pub struct Network {
    /// Radio's profile ID for this access point.
    pub id: u32,
    pub channel: u32,
    pub security: Security,
    pub ssid: Vec<u8>,
}

impl Network {
    /// The name to show (an SSID is bytes, almost always UTF-8).
    pub fn name(&self) -> String {
        let s = String::from_utf8_lossy(&self.ssid).into_owned();
        if s.trim().is_empty() { "(hidden network)".to_string() } else { s }
    }
    pub fn band(&self) -> &'static str {
        if self.channel == 0 {
            ""
        } else if self.channel <= 14 {
            "2.4 GHz"
        } else {
            "5 GHz"
        }
    }
}

/// What the runtime reports in "wifi".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// "scanning", "joining" or "leaving" while a request runs.
    pub busy: Option<String>,
    /// The joined network's profile ID, 0 for none.
    pub connected: u32,
    pub ssid: Vec<u8>,
    /// Radio is hosting Phone Internet instead of joining a network.
    pub hosting: bool,
    /// The last request and its outcome: ("join", false, reason).
    pub last: Option<(String, bool, u32)>,
    /// Settings › Radios has Wi-Fi off: no scans or joins until it is on.
    pub off: bool,
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn state() -> State {
    let f = fields(&sys("wifi"));
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    State {
        busy: Some(get("state")).filter(|s| !s.is_empty() && *s != "idle").map(String::from),
        connected: get("connected").parse().unwrap_or(0),
        ssid: unhex(get("ssid")).unwrap_or_default(),
        hosting: get("hosting") == "1",
        last: match get("last") {
            "" | "-" => None,
            what => Some((what.to_string(), get("result") == "ok", get("reason").parse().unwrap_or(0))),
        },
        off: get("radio") == "off",
    }
}

impl State {
    /// Whether `n` is the joined network. Radio tries every access point with
    /// the chosen name, so the one it joined can differ from the one chosen.
    pub fn is_joined(&self, n: &Network) -> bool {
        self.connected != 0 && (n.id == self.connected || (!self.ssid.is_empty() && n.ssid == self.ssid))
    }
}

/// Start a scan; poll `state()` until it is no longer busy, then read `networks()`.
pub fn scan() -> Result<(), String> {
    answer(sys("wifi scan"))
}

/// The networks from the last scan, one entry per name (the first access point
/// seen for it: Radio tries each one with that name when joining).
pub fn networks() -> Vec<Network> {
    let mut out: Vec<Network> = Vec::new();
    for line in sys("wifi list").lines() {
        let f = fields(line);
        let Some(id) = f.get("id").and_then(|v| v.parse().ok()) else { continue };
        let ssid = f.get("ssid").and_then(|v| unhex(v)).unwrap_or_default();
        if !ssid.is_empty() && out.iter().any(|n| n.ssid == ssid) {
            continue;
        }
        out.push(Network {
            id,
            channel: f.get("channel").and_then(|v| v.parse().ok()).unwrap_or(0),
            security: match f.get("security").map(String::as_str) {
                Some("wpa2") => Security::Wpa2,
                Some("open") => Security::Open,
                _ => Security::Other,
            },
            ssid,
        });
    }
    out
}

/// Start joining network `id`, with its WPA2 key when it has one.
pub fn join(id: u32, key: Option<&[u8; 32]>) -> Result<(), String> {
    match key {
        Some(k) => answer(sys(&format!("wifi join {id} {}", hex(k)))),
        None => answer(sys(&format!("wifi join {id}"))),
    }
}

pub fn leave() -> Result<(), String> {
    answer(sys("wifi leave"))
}

fn answer(a: String) -> Result<(), String> {
    if a == "ok" { Ok(()) } else { Err(a.trim_start_matches("error ").to_string()) }
}

/// The WPA2 pairwise master key: PBKDF2-HMAC-SHA1 of the passphrase, salted
/// with the SSID, 4096 rounds (IEEE 802.11i). The password never leaves the app.
pub fn pmk(ssid: &[u8], passphrase: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        NonZeroU32::new(4096).unwrap(),
        ssid,
        passphrase.as_bytes(),
        &mut key,
    );
    key
}

/// A WPA2 passphrase is 8 to 63 printable ASCII characters.
pub fn valid_passphrase(p: &str) -> bool {
    (8..=63).contains(&p.len()) && p.bytes().all(|b| (32..127).contains(&b))
}

/// Why a join failed, for people (reasons from runtime/protocol.rs).
pub fn reason(code: u32) -> &'static str {
    match code {
        52 => "Wrong password. Check it and try again.",
        53 => "Connected, but the network gave no address.",
        49 => "The network is out of range. Scan again.",
        47 | 48 => "The network didn't answer. Move closer and try again.",
        57 => "The network didn't finish connecting. Try again.",
        50 => "This network needs its password.",
        2 => "Radio refused this network.",
        3 => "Wi-Fi is unavailable right now.",
        _ => "Couldn't connect. Try again.",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pmk_matches_ieee_test_vector() {
        // IEEE 802.11i-2004, H.4.1: "password" and "IEEE".
        let key = super::pmk(b"IEEE", "password");
        assert_eq!(super::hex(&key), "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
    }
}
