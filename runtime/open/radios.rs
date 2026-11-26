//! Settings › Radios: the owner's switches for the phone's radios ("radios"
//! in /dev/violet/sys).
//!
//!   radios                  wifi=on|off bluetooth=on|off cellular=none gps=none
//!   radios wifi on|off      (and bluetooth; "radios all off" turns both off)
//!
//! Only Settings may change them. The choice is kept in /data/radios, which
//! open apps may read but not write, rename or remove, and the runtime applies
//! it at start, before the auto-connector or Phone Internet look.
//!
//! - Wi-Fi off: Radio leaves any network and stops Phone Internet, which
//!   powers the connectivity chip down (Bluetooth with it); scans, joins,
//!   Phone Internet and the auto-connector are refused until it is on again.
//! - Bluetooth off: the controller is reset and no app may open
//!   /dev/violet/hci; an app holding it sees the controller off.
//! - Cellular and GPS: VioletOS has no driver for either and never starts them.
use alloc::string::String;

pub const SAVED: &str = "/data/radios";

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Radios {
    pub wifi: bool,
    pub bluetooth: bool,
}

impl Default for Radios {
    fn default() -> Self {
        Radios { wifi: true, bluetooth: true }
    }
}

impl Radios {
    /// From /data/radios ("wifi=0" and "bluetooth=0" lines; anything missing is on).
    pub fn parse(text: &str) -> Radios {
        let mut r = Radios::default();
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let on = v.trim() != "0";
            match k.trim() {
                "wifi" => r.wifi = on,
                "bluetooth" => r.bluetooth = on,
                _ => {}
            }
        }
        r
    }

    pub fn text(&self) -> String {
        alloc::format!("wifi={}\nbluetooth={}\n", self.wifi as u8, self.bluetooth as u8)
    }

    pub fn report(&self) -> String {
        let s = |on: bool| if on { "on" } else { "off" };
        alloc::format!("wifi={} bluetooth={} cellular=none gps=none\n", s(self.wifi), s(self.bluetooth))
    }

    /// Applies "wifi on", "bluetooth off" or "all off"; None for anything else.
    pub fn change(&self, what: &str, state: &str) -> Option<Radios> {
        let on = match state {
            "on" => true,
            "off" => false,
            _ => return None,
        };
        let mut r = *self;
        match what {
            "wifi" => r.wifi = on,
            "bluetooth" => r.bluetooth = on,
            "all" if !on => r = Radios { wifi: false, bluetooth: false },
            _ => return None,
        }
        Some(r)
    }
}

/// Whether an open app may change `path`: none may change the saved radios.
pub fn app_may_write(path: &str) -> bool {
    path != SAVED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_choice_round_trips() {
        let r = Radios { wifi: false, bluetooth: true };
        assert_eq!(Radios::parse(&r.text()), r);
        // A missing or damaged file leaves the radios on.
        assert_eq!(Radios::parse(""), Radios::default());
        assert_eq!(Radios::parse("junk\nwifi\n"), Radios::default());
    }

    #[test]
    fn changes() {
        let on = Radios::default();
        assert_eq!(on.change("wifi", "off"), Some(Radios { wifi: false, bluetooth: true }));
        assert_eq!(on.change("all", "off"), Some(Radios { wifi: false, bluetooth: false }));
        assert_eq!(on.change("all", "on"), None);
        assert_eq!(on.change("cellular", "on"), None);
        assert_eq!(on.change("wifi", "maybe"), None);
    }
}
