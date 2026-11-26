//! The Settings pages other than Wi-Fi. Everything is drawn from `View`, so a
//! preview can be drawn without the devices.
use violet::paper::{slab, ALERT, CREAM, GOOD, MUTED, PAPER, PAPER_PRESSED, PAPER_RAISED, VIOLET};
use violet::{Canvas, Fonts, Rect, CITIES, HOME_BOX, W};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    Menu,
    Wifi,
    Bluetooth,
    Radios,
    PhoneLink,
    Time,
    AgentApps,
    About,
}

/// The menu, in order.
pub const MENU: [(Section, &str); 7] = [
    (Section::Wifi, "Wi-Fi"),
    (Section::Bluetooth, "Bluetooth"),
    (Section::Radios, "Radios"),
    (Section::PhoneLink, "Phone Link"),
    (Section::Time, "Date & Time"),
    (Section::AgentApps, "Agent Apps"),
    (Section::About, "About"),
];

/// Something under the finger, drawn pressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Back,
    Row(usize),
    Relay,
    NewCode,
    City(usize),
    Delete(usize),
    DeleteAll,
    /// Radios: a radio's switch and its i, and All off.
    Switch(usize),
    Info(usize),
    AllOff,
}

/// The radios, as Settings › Radios lists them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Radio {
    Wifi,
    Bluetooth,
    Cellular,
    Gps,
}
pub const RADIOS: [Radio; 4] = [Radio::Wifi, Radio::Bluetooth, Radio::Cellular, Radio::Gps];

impl Radio {
    pub fn name(self) -> &'static str {
        match self {
            Radio::Wifi => "Wi-Fi",
            Radio::Bluetooth => "Bluetooth",
            Radio::Cellular => "Cellular",
            Radio::Gps => "GPS",
        }
    }
    /// What it does, and what Off really means, for its i.
    pub fn about(self) -> &'static str {
        match self {
            Radio::Wifi => "Joins Wi-Fi networks and hosts Phone Internet. Off: Radio leaves the network, stops Phone Internet and powers the connectivity chip down. Nothing scans or rejoins until you turn it on. Bluetooth shares the chip, so it goes quiet too.",
            Radio::Bluetooth => "Lets apps such as Bitchat talk to devices nearby. Off: the controller is reset, so it stops advertising and scanning, and no app can use it. It shares the chip with Wi-Fi, which keeps it powered while Wi-Fi is on. Turn Wi-Fi off to power it down.",
            Radio::Cellular => "Calls, texts and mobile data. VioletOS has no driver for the modem and never starts it, so it stays off. Phone Internet borrows your iPhone's cellular data over Wi-Fi instead.",
            Radio::Gps => "Satellite location. VioletOS has no driver for it and never turns it on, so no app can learn where you are from it.",
        }
    }
    /// VioletOS can switch it; the others are always off.
    pub fn switchable(self) -> bool {
        matches!(self, Radio::Wifi | Radio::Bluetooth)
    }
}

/// An app Violet Agent installed.
#[derive(Clone, PartialEq, Debug)]
pub struct Installed {
    pub name: String,
    pub title: String,
    pub bytes: u64,
}

/// "name<TAB>bytes<TAB>title[<TAB>package]" lines from "installed": the apps,
/// leaving out packages (Python, which the apps need) and the ".free" line.
pub fn parse_installed(text: &str) -> Vec<Installed> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (name, bytes, title) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            if name.starts_with('.') || f.next() == Some("package") {
                return None;
            }
            Some(Installed { name: name.into(), bytes: bytes.parse().ok()?, title: if title.is_empty() { name.into() } else { title.into() } })
        })
        .collect()
}

#[derive(Clone, Default, PartialEq)]
pub struct Status {
    pub wifi: String,
    pub bt_on: bool,
    pub bt_open: bool,
    pub shield_known: bool,
    pub shield_linked: bool,
    pub shield_rules: u32,
    pub frozen: Vec<String>,
    /// Phone Internet: "off", "starting", "waiting", "joined", "online", "unavailable" or "busy".
    pub relay: String,
    pub relay_failure: u32,
    pub battery: i32,
    pub volume: i32,
    pub city: usize,
    pub installed: Vec<Installed>,
    /// Settings › Radios: Wi-Fi and Bluetooth allowed on ("radios" in /dev/violet/sys).
    pub radio_wifi: bool,
    pub radio_bluetooth: bool,
}

impl Status {
    pub fn radio_on(&self, r: Radio) -> bool {
        match r {
            Radio::Wifi => self.radio_wifi,
            Radio::Bluetooth => self.radio_bluetooth,
            Radio::Cellular | Radio::Gps => false,
        }
    }
}

pub struct View<'a> {
    pub section: Section,
    pub status: &'a Status,
    /// The saved Phone Internet pairing code and whether it should be on.
    pub code: &'a str,
    pub relay_on: bool,
    pub pressed: Option<Hit>,
    /// The app whose Delete was tapped once (usize::MAX: Delete all); on
    /// Radios, the radio whose i is open.
    pub confirm: Option<usize>,
    pub note: &'a str,
}

const MENU_TOP: i32 = 250;
const MENU_ROW: i32 = 124;

pub fn menu_rect(i: usize) -> Rect {
    let y = MENU_TOP + i as i32 * MENU_ROW;
    Rect::new(40, y, W as i32 - 40, y + MENU_ROW - 16)
}
pub const RELAY_BUTTON: Rect = Rect::new(40, 1010, W as i32 - 40, 1110);
pub const NEW_CODE: Rect = Rect::new(40, 1140, W as i32 - 40, 1240);
const CITY_TOP: i32 = 250;
const CITY_ROW: i32 = 118;
pub fn city_rect(i: usize) -> Rect {
    let (col, row) = (i as i32 % 2, i as i32 / 2);
    let x = 40 + col * 330;
    let y = CITY_TOP + row * CITY_ROW;
    Rect::new(x, y, x + 310, y + CITY_ROW - 16)
}
const APPS_TOP: i32 = 250;
const APP_ROW: i32 = 124;
pub const MAX_LISTED: usize = 8;
pub fn app_rect(i: usize) -> Rect {
    let y = APPS_TOP + i as i32 * APP_ROW;
    Rect::new(40, y, W as i32 - 40, y + APP_ROW - 16)
}
pub fn delete_rect(i: usize) -> Rect {
    let r = app_rect(i);
    Rect::new(r.x1 - 196, r.y0 + 20, r.x1 - 20, r.y1 - 20)
}
pub const DELETE_ALL: Rect = Rect::new(40, 1300, W as i32 - 40, 1400);
const RADIO_TOP: i32 = 250;
const RADIO_ROW: i32 = 150;
pub fn radio_rect(i: usize) -> Rect {
    let y = RADIO_TOP + i as i32 * RADIO_ROW;
    Rect::new(40, y, W as i32 - 40, y + RADIO_ROW - 20)
}
pub fn switch_rect(i: usize) -> Rect {
    let r = radio_rect(i);
    let y = r.y0 + (r.height() - 56) / 2;
    Rect::new(r.x1 - 32 - violet::paper::SWITCH_W, y, r.x1 - 32, y + 56)
}
pub fn info_rect(i: usize) -> Rect {
    let s = switch_rect(i);
    let cy = (s.y0 + s.y1) / 2;
    Rect::new(s.x0 - 96, cy - 34, s.x0 - 28, cy + 34)
}
pub const ALL_OFF: Rect = Rect::new(40, 1440, W as i32 - 40, 1540);

pub fn hit(v: &View, x: i32, y: i32) -> Option<Hit> {
    if HOME_BOX.contains(x, y) {
        return Some(Hit::Back);
    }
    match v.section {
        Section::Menu => (0..MENU.len()).find(|&i| menu_rect(i).contains(x, y)).map(Hit::Row),
        Section::PhoneLink if relay_usable(v.status) => {
            if RELAY_BUTTON.contains(x, y) {
                Some(Hit::Relay)
            } else if !v.relay_on && NEW_CODE.contains(x, y) {
                Some(Hit::NewCode)
            } else {
                None
            }
        }
        Section::Time => (0..CITIES.len()).find(|&i| city_rect(i).contains(x, y)).map(Hit::City),
        Section::Radios => {
            if ALL_OFF.contains(x, y) && (v.status.radio_wifi || v.status.radio_bluetooth) {
                return Some(Hit::AllOff);
            }
            (0..RADIOS.len()).find_map(|i| {
                if info_rect(i).inset(-10).contains(x, y) {
                    Some(Hit::Info(i))
                } else if RADIOS[i].switchable() && switch_rect(i).inset(-16).contains(x, y) {
                    Some(Hit::Switch(i))
                } else {
                    None
                }
            })
        }
        Section::AgentApps => {
            let n = v.status.installed.len().min(MAX_LISTED);
            if n > 0 && DELETE_ALL.contains(x, y) {
                return Some(Hit::DeleteAll);
            }
            (0..n).find(|&i| delete_rect(i).contains(x, y)).map(Hit::Delete)
        }
        _ => None,
    }
}

fn relay_usable(s: &Status) -> bool {
    s.relay != "unavailable"
}

fn header(c: &mut Canvas, f: &Fonts, v: &View, back: &str, title: &str) {
    let bg = if v.pressed == Some(Hit::Back) { PAPER_PRESSED } else { PAPER_RAISED };
    violet::button(c, &f.medium(24.0), HOME_BOX, back, bg, CREAM);
    f.regular(48.0).draw(c, title, 40, 180, CREAM);
}

fn chevron(c: &mut Canvas, x: f32, y: f32) {
    c.line(x - 6.0, y - 13.0, x + 5.0, y, 4.0, MUTED);
    c.line(x + 5.0, y, x - 6.0, y + 13.0, 4.0, MUTED);
}

/// A paragraph wrapped to `width`; returns the y after it.
fn paragraph(c: &mut Canvas, f: &violet::Face, text: &str, x: i32, mut y: i32, width: i32, ink: violet::Color) -> i32 {
    let mut line = String::new();
    for word in text.split_whitespace() {
        let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if f.width(&next) > width && !line.is_empty() {
            f.draw(c, &line, x, y, ink);
            y += (f.size() * 1.4) as i32;
            line = word.to_string();
        } else {
            line = next;
        }
    }
    if !line.is_empty() {
        f.draw(c, &line, x, y, ink);
        y += (f.size() * 1.4) as i32;
    }
    y
}

pub fn menu_value(s: &Status, section: Section) -> (String, bool) {
    match section {
        Section::Wifi => (s.wifi.clone(), s.wifi != "Not connected"),
        Section::Bluetooth => ((if s.bt_on { "On" } else { "Off" }).into(), s.bt_on),
        Section::PhoneLink => {
            let shield = if !s.shield_known { "" } else if s.shield_linked { "Shield linked" } else { "Shield waiting" };
            let relay = match s.relay.as_str() {
                "online" => "Internet on",
                "waiting" | "joined" | "starting" => "Internet starting",
                _ => "",
            };
            let parts: Vec<&str> = [shield, relay].into_iter().filter(|p| !p.is_empty()).collect();
            (if parts.is_empty() { "Not linked".into() } else { parts.join(" · ") }, s.shield_linked || s.relay == "online")
        }
        Section::Time => (CITIES[s.city.min(CITIES.len() - 1)].into(), false),
        Section::Radios => (
            match (s.radio_wifi, s.radio_bluetooth) {
                (true, true) => "Wi-Fi, Bluetooth on".into(),
                (true, false) => "Bluetooth off".into(),
                (false, true) => "Wi-Fi off".into(),
                (false, false) => "All off".into(),
            },
            false,
        ),
        Section::AgentApps => {
            let n = s.installed.len();
            (if n == 0 { "None".into() } else { format!("{n} installed") }, false)
        }
        _ => (String::new(), false),
    }
}

pub fn draw(c: &mut Canvas, f: &Fonts, v: &View) {
    c.fill(c.bounds(), PAPER);
    let s = v.status;
    match v.section {
        Section::Menu => {
            header(c, f, v, "‹ Home", "Settings");
            let (title, value) = (f.medium(34.0), f.regular(27.0));
            for (i, (section, name)) in MENU.iter().enumerate() {
                let r = menu_rect(i);
                slab(c, r, 26.0, if v.pressed == Some(Hit::Row(i)) { PAPER_PRESSED } else { PAPER_RAISED });
                let base = r.y0 + r.height() / 2 + 12;
                title.draw(c, name, r.x0 + 32, base, CREAM);
                let (text, lit) = menu_value(s, *section);
                let w = value.width(&text).min(300);
                value.draw(c, &value.fit(&text, 300), r.x1 - 60 - w, base - 1, if lit { VIOLET } else { MUTED });
                chevron(c, (r.x1 - 34) as f32, (r.y0 + r.height() / 2) as f32);
            }
        }
        Section::Wifi => {}
        Section::Bluetooth => {
            header(c, f, v, "‹ Settings", "Bluetooth");
            let card = Rect::new(40, 230, W as i32 - 40, 390);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(36.0).draw(c, if s.bt_on { "On" } else { "Off" }, 80, 300, if s.bt_on { VIOLET } else { CREAM });
            let line = if !s.radio_bluetooth {
                "Turned off in Settings › Radios"
            } else if s.bt_open {
                "In use by Bitchat"
            } else if s.bt_on {
                "Ready for nearby chat"
            } else {
                "The radio is not running"
            };
            f.regular(27.0).draw(c, line, 80, 346, MUTED);
            paragraph(c, &f.regular(28.0), "Bluetooth shares the phone's connectivity chip with Wi-Fi, so it runs while Wi-Fi is on. Bitchat uses it to chat with phones nearby, with no internet needed.", 44, 460, W as i32 - 88, MUTED);
        }
        Section::PhoneLink => {
            header(c, f, v, "‹ Settings", "Phone Link");
            // Violet Shield.
            let card = Rect::new(40, 230, W as i32 - 40, 420);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(23.0).draw(c, "VIOLET SHIELD", 80, 284, MUTED);
            let (head, tint) = if !s.frozen.is_empty() {
                (format!("Froze {}", s.frozen.join(", ")), ALERT)
            } else if s.shield_linked {
                ("Linked to your Mac".to_string(), VIOLET)
            } else {
                ("Waiting for your Mac".to_string(), CREAM)
            };
            let big = f.medium(34.0);
            big.draw(c, &big.fit(&head, card.width() - 80), 80, 336, tint);
            let rules = format!("{} rule{} in force", s.shield_rules, if s.shield_rules == 1 { "" } else { "s" });
            f.regular(26.0).draw(c, if s.shield_known { &rules } else { "Not running on this phone" }, 80, 382, MUTED);
            // Phone Internet.
            let card = Rect::new(40, 450, W as i32 - 40, 960);
            slab(c, card, 30.0, PAPER_RAISED);
            f.medium(23.0).draw(c, "PHONE INTERNET", 80, 504, MUTED);
            let state = match s.relay.as_str() {
                "online" => "Online through your phone",
                "joined" => "Your phone joined",
                "waiting" => "Waiting for your phone",
                "starting" => "Starting…",
                "unavailable" => "Not available on this phone",
                "busy" => "Checking…",
                _ if v.relay_on && s.relay_failure != 0 => "Couldn't start",
                _ => "Off",
            };
            big.draw(c, state, 80, 556, if s.relay == "online" { GOOD } else { CREAM });
            let mut y = 620;
            if relay_usable(s) {
                f.medium(23.0).draw(c, "PAIRING CODE", 80, y, MUTED);
                let spaced = if v.code.len() == 12 { format!("{} {} {}", &v.code[..4], &v.code[4..8], &v.code[8..]) } else { "—".into() };
                f.medium(52.0).draw(c, &spaced, 80, y + 70, CREAM);
                y += 130;
            }
            paragraph(c, &f.regular(26.0), "Open Violet Relay on your iPhone, enter the code and tap Join. This phone then reaches the internet over your iPhone's cellular data.", 80, y, card.width() - 80, MUTED);
            if relay_usable(s) {
                let on = v.relay_on;
                let bg = if v.pressed == Some(Hit::Relay) { PAPER_PRESSED } else if on { PAPER_RAISED } else { VIOLET };
                violet::button(c, &f.medium(34.0), RELAY_BUTTON, if on { "Turn off" } else { "Turn on" }, bg, if on { CREAM } else { PAPER });
                if !on {
                    let bg = if v.pressed == Some(Hit::NewCode) { PAPER_PRESSED } else { PAPER_RAISED };
                    violet::button(c, &f.medium(30.0), NEW_CODE, "New code", bg, CREAM);
                }
            }
            if !v.note.is_empty() {
                f.regular(26.0).centered(c, v.note, 0, W as i32, 1290, ALERT);
            }
        }
        Section::Radios => radios(c, f, v),
        Section::Time => {
            header(c, f, v, "‹ Settings", "Date & Time");
            f.regular(26.0).draw(c, "Home shows the date where you are", 44, 228, MUTED);
            let face = f.medium(28.0);
            for (i, city) in CITIES.iter().enumerate() {
                let r = city_rect(i);
                let chosen = s.city == i;
                slab(c, r, 22.0, if v.pressed == Some(Hit::City(i)) { PAPER_PRESSED } else { PAPER_RAISED });
                face.draw(c, &face.fit(city, r.width() - 80), r.x0 + 26, r.y0 + r.height() / 2 + 10, if chosen { VIOLET } else { CREAM });
                if chosen {
                    let (cx, cy) = ((r.x1 - 36) as f32, (r.y0 + r.height() / 2) as f32);
                    c.line(cx - 12.0, cy, cx - 3.0, cy + 9.0, 5.0, VIOLET);
                    c.line(cx - 3.0, cy + 9.0, cx + 13.0, cy - 10.0, 5.0, VIOLET);
                }
            }
        }
        Section::AgentApps => {
            header(c, f, v, "‹ Settings", "Agent Apps");
            f.regular(26.0).draw(c, "Apps Violet Agent built for you", 44, 228, MUTED);
            if s.installed.is_empty() {
                paragraph(c, &f.regular(30.0), "No apps yet. Ask Violet Agent for one and it appears on Home.", 44, 340, W as i32 - 88, MUTED);
            }
            let (title, small) = (f.medium(32.0), f.regular(24.0));
            for (i, a) in s.installed.iter().take(MAX_LISTED).enumerate() {
                let r = app_rect(i);
                slab(c, r, 26.0, PAPER_RAISED);
                let d = delete_rect(i);
                title.draw(c, &title.fit(&a.title, d.x0 - r.x0 - 60), r.x0 + 30, r.y0 + 50, CREAM);
                small.draw(c, &format!("{} KB", (a.bytes + 1023) / 1024), r.x0 + 30, r.y0 + 88, MUTED);
                let confirm = v.confirm == Some(i);
                let bg = if v.pressed == Some(Hit::Delete(i)) { PAPER_PRESSED } else if confirm { ALERT } else { PAPER };
                violet::button(c, &f.medium(26.0), d, if confirm { "Confirm" } else { "Delete" }, bg, if confirm { PAPER } else { ALERT });
            }
            if s.installed.len() > MAX_LISTED {
                small.draw(c, &format!("and {} more", s.installed.len() - MAX_LISTED), 44, app_rect(MAX_LISTED).y0 + 30, MUTED);
            }
            if !s.installed.is_empty() {
                let confirm = v.confirm == Some(usize::MAX);
                let bg = if v.pressed == Some(Hit::DeleteAll) { PAPER_PRESSED } else if confirm { ALERT } else { PAPER_RAISED };
                let label = if confirm { "Tap again to delete all".to_string() } else { format!("Delete all {}", s.installed.len()) };
                violet::button(c, &f.medium(32.0), DELETE_ALL, &label, bg, if confirm { PAPER } else { ALERT });
            }
            if !v.note.is_empty() {
                f.regular(26.0).centered(c, v.note, 0, W as i32, 1460, ALERT);
            }
        }
        Section::About => {
            header(c, f, v, "‹ Settings", "About");
            let card = Rect::new(40, 230, W as i32 - 40, 700);
            slab(c, card, 30.0, PAPER_RAISED);
            let (label, value) = (f.regular(27.0), f.medium(30.0));
            let rows = [
                ("System", "VioletOS".to_string()),
                ("Kernel", "seL4 microkernel".to_string()),
                ("Battery", if s.battery >= 0 { format!("{}%", s.battery) } else { "—".into() }),
                ("Volume", if s.volume >= 0 { s.volume.to_string() } else { "—".into() }),
                ("Agent apps", s.installed.len().to_string()),
                ("Wi-Fi", s.wifi.clone()),
            ];
            for (k, (name, v)) in rows.iter().enumerate() {
                let y = 300 + k as i32 * 66;
                label.draw(c, name, 80, y, MUTED);
                value.draw(c, &value.fit(v, card.x1 - 40 - 300), 300, y, CREAM);
            }
        }
    }
}

/// Every radio in the phone, each with what it does (i) and a switch; the
/// ones VioletOS never starts show a switch locked off.
fn radios(c: &mut Canvas, f: &Fonts, v: &View) {
    let s = v.status;
    header(c, f, v, "‹ Settings", "Radios");
    f.regular(26.0).draw(c, "Turn off any radio you don't want on.", 44, 228, MUTED);
    let (name, small) = (f.medium(34.0), f.regular(24.0));
    for (i, &r) in RADIOS.iter().enumerate() {
        let card = radio_rect(i);
        slab(c, card, 26.0, PAPER_RAISED);
        let on = s.radio_on(r);
        name.draw(c, r.name(), card.x0 + 32, card.y0 + 58, if r.switchable() { CREAM } else { MUTED });
        let status = match r {
            Radio::Wifi if on => s.wifi.clone(),
            Radio::Wifi => "Chip powered down".into(),
            Radio::Bluetooth if !on => "No app can use it".into(),
            Radio::Bluetooth if !s.radio_wifi => "Quiet while Wi-Fi is off".into(),
            Radio::Bluetooth if s.bt_open => "In use by an app".into(),
            Radio::Bluetooth => "Ready".into(),
            Radio::Cellular | Radio::Gps => "No driver · never started".into(),
        };
        small.draw(c, &small.fit(&status, info_rect(i).x0 - card.x0 - 48), card.x0 + 32, card.y0 + 100, MUTED);
        // The i.
        let ir = info_rect(i);
        let open = v.confirm == Some(i);
        let (cx, cy) = (((ir.x0 + ir.x1) / 2) as f32, ((ir.y0 + ir.y1) / 2) as f32);
        let ring = if v.pressed == Some(Hit::Info(i)) || open { VIOLET } else { MUTED };
        c.circle(cx, cy, 22.0, ring);
        c.circle(cx, cy, 19.0, if open { VIOLET } else { PAPER_RAISED });
        f.medium(28.0).centered(c, "i", ir.x0, ir.x1, cy as i32 + 10, if open { PAPER } else { CREAM });
        let sw = switch_rect(i);
        violet::paper::switch(c, sw.x0, sw.y0, on);
        if !r.switchable() {
            // Locked off: VioletOS can't turn it on.
            c.round_rect_alpha(sw.inset(-2), 28.0, PAPER_RAISED, 0.45);
        }
    }
    let top = radio_rect(RADIOS.len() - 1).y1 + 40;
    match v.confirm.and_then(|i| RADIOS.get(i)) {
        Some(&r) => {
            f.medium(30.0).draw(c, r.name(), 44, top + 20, CREAM);
            paragraph(c, &f.regular(26.0), r.about(), 44, top + 66, W as i32 - 88, MUTED);
        }
        None => {
            f.regular(26.0).draw(c, "Tap i to see what a radio does and what Off means.", 44, top + 20, MUTED);
        }
    }
    let any = s.radio_wifi || s.radio_bluetooth;
    let bg = if v.pressed == Some(Hit::AllOff) { PAPER_PRESSED } else { PAPER_RAISED };
    violet::button(c, &f.medium(32.0), ALL_OFF, if any { "All off" } else { "All radios are off" }, bg, if any { ALERT } else { MUTED });
    if !v.note.is_empty() {
        f.regular(26.0).centered(c, v.note, 0, W as i32, ALL_OFF.y0 - 30, ALERT);
    }
}
