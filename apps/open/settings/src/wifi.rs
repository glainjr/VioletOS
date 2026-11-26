//! Settings › Wi-Fi: scan for networks, choose one, type its password and join it.
//!
//! The password is turned into the network's WPA2 key here (violet::wifi::pmk)
//! and only the key goes to Radio. Keys of joined networks are kept in
//! /data/networks, so a known network joins with one tap.
use std::time::{Duration, Instant};

use violet::keyboard::{self, Key, Keyboard};
use crate::wifi_ui::{self as ui, Hit, Page};
use violet::device::{TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP};
use violet::wifi::{self, Network, Security};
use violet::{Canvas, Fonts};

const SAVED: &str = "/data/networks";

/// Known networks, most recently joined first: SSID and its WPA2 key (None for an
/// open network). Stored one per line as "SSID KEY" in hex, or "SSID open"; the
/// Open Runtime reads the same file to rejoin them on its own after a restart.
#[derive(Default)]
struct Saved(Vec<(Vec<u8>, Option<[u8; 32]>)>);

impl Saved {
    fn load() -> Saved {
        let mut s = Saved::default();
        for line in std::fs::read_to_string(SAVED).unwrap_or_default().lines() {
            let mut w = line.split_whitespace();
            let (Some(ssid), Some(key)) = (w.next().and_then(unhex), w.next()) else { continue };
            let key = if key == "open" {
                None
            } else {
                match unhex(key).and_then(|k| <[u8; 32]>::try_from(k).ok()) {
                    Some(k) => Some(k),
                    None => continue,
                }
            };
            if s.get(&ssid).is_none() {
                s.0.push((ssid, key));
            }
        }
        s
    }
    fn store(&self) {
        let text: String = self
            .0
            .iter()
            .map(|(ssid, key)| match key {
                Some(k) => format!("{} {}\n", wifi::hex(ssid), wifi::hex(k)),
                None => format!("{} open\n", wifi::hex(ssid)),
            })
            .collect();
        if let Err(e) = std::fs::write(SAVED, text) {
            println!("wifi: could not save networks: {e}");
        }
    }
    fn get(&self, ssid: &[u8]) -> Option<&Option<[u8; 32]>> {
        self.0.iter().find(|(s, _)| s == ssid).map(|(_, k)| k)
    }
    fn contains(&self, ssid: &[u8]) -> bool {
        self.get(ssid).is_some()
    }
    /// Keep `ssid` (with its key) as the most recently joined network.
    fn put_first(&mut self, ssid: &[u8], key: Option<[u8; 32]>) {
        self.remove(ssid);
        self.0.insert(0, (ssid.to_vec(), key));
    }
    fn remove(&mut self, ssid: &[u8]) -> bool {
        let before = self.0.len();
        self.0.retain(|(s, _)| s != ssid);
        self.0.len() != before
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then_some(())?;
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A join this app asked for.
struct Joining {
    net: Network,
    key: Option<[u8; 32]>,
    /// The password typed for it (None: a saved key or an open network).
    typed: Option<String>,
}

struct Touch {
    start: (i32, i32),
    /// Where the finger was last seen: a lift reports no position of its own.
    last: (i32, i32),
    scroll0: i32,
    dragging: bool,
}

pub struct App {
    /// The person went back to the Settings menu.
    pub exit: bool,
    page: Page,
    nets: Vec<Network>,
    state: wifi::State,
    saved: Saved,
    joining: Option<Joining>,
    /// A leave this app asked for.
    leaving: bool,
    scanning: bool,
    message: Option<String>,
    scroll: i32,
    touch: Option<Touch>,
    pressed: Option<Hit>,
    keyboard: Keyboard,
    /// When the backspace key went down, for repeating while held.
    held_back: Option<Instant>,
    started: Instant,
}

impl App {
    pub fn new() -> App {
        let app = App {
            exit: false,
            page: Page::List,
            nets: wifi::networks(),
            state: wifi::state(),
            saved: Saved::load(),
            joining: None,
            leaving: false,
            scanning: false,
            message: None,
            scroll: 0,
            touch: None,
            pressed: None,
            keyboard: Keyboard::default(),
            held_back: None,
            started: Instant::now(),
        };
        println!("wifi: ready, {} saved networks", app.saved.0.len());
        app
    }

    /// Coming (back) to the Wi-Fi page.
    pub fn enter(&mut self) {
        self.exit = false;
        self.page = Page::List;
        self.pressed = None;
        self.keyboard.pressed = None;
        self.refresh();
        self.message = None;
        self.scan();
    }

    pub fn draw(&self, c: &mut Canvas, f: &Fonts) {
        ui::draw(c, f, &self.view());
    }

    /// Wants frequent redraws: a spinner turns or backspace repeats.
    pub fn animating(&self) -> bool {
        self.busy() || self.held_back.is_some()
    }

    /// The periodic part of the loop: repeat backspace, follow the runtime.
    /// True when the screen needs drawing.
    pub fn tick(&mut self, last_poll: &mut Instant) -> bool {
        let mut dirty = false;
        if let Some(t) = self.held_back {
            if t.elapsed() > Duration::from_millis(450) {
                self.type_key(Key::Back);
                self.held_back = Some(t + Duration::from_millis(70));
                dirty = true;
            }
        }
        if last_poll.elapsed() >= if self.busy() { Duration::from_millis(250) } else { Duration::from_secs(3) } {
            *last_poll = Instant::now();
            dirty |= self.refresh();
        }
        dirty | self.busy()
    }

    /// The joined network's name, or Phone Internet, for the Settings menu.
    pub fn summary() -> String {
        let s = wifi::state();
        if s.off {
            "Off".into()
        } else if s.hosting {
            "Phone Internet".into()
        } else if s.connected != 0 && !s.ssid.is_empty() {
            String::from_utf8_lossy(&s.ssid).into_owned()
        } else if s.connected != 0 {
            "Connected".into()
        } else {
            "Not connected".into()
        }
    }

    fn view(&self) -> ui::View<'_> {
        ui::View {
            page: &self.page,
            nets: &self.nets,
            state: &self.state,
            connected_name: String::from_utf8_lossy(&self.state.ssid).into_owned(),
            hosting: self.state.hosting,
            busy: self.state.busy.as_deref(),
            joining: self.joining.as_ref().map(|j| j.net.id),
            saved: self.saved.0.iter().map(|(s, _)| s.as_slice()).collect(),
            message: self.message.as_deref(),
            scroll: self.scroll,
            pressed: self.pressed,
            keyboard: &self.keyboard,
            phase: (self.started.elapsed().as_millis() % 1000) as f32 / 1000.0,
        }
    }

    fn busy(&self) -> bool {
        self.state.busy.is_some() || self.joining.is_some() || self.leaving || self.scanning
    }

    fn scan(&mut self) {
        if self.busy() {
            return;
        }
        match wifi::scan() {
            Ok(()) => {
                self.scanning = true;
                self.state.busy = Some("scanning".into());
                println!("wifi: scanning");
            }
            Err(e) if e == "busy" => {} // the runtime is already rejoining a saved network
            Err(e) if e.starts_with("off") => self.message = Some("Wi-Fi is off. Turn it on in Settings › Radios.".into()),
            Err(e) => self.message = Some(format!("Couldn't scan: {e}")),
        }
    }

    fn join(&mut self, net: Network, key: Option<[u8; 32]>, typed: Option<String>) {
        if self.busy() {
            return;
        }
        println!("wifi: joining {}", net.name());
        match wifi::join(net.id, key.as_ref()) {
            Ok(()) => {
                self.message = None;
                self.state.busy = Some("joining".into());
                self.joining = Some(Joining { net, key, typed });
            }
            Err(e) if e == "busy" => self.message = Some("Wi-Fi is busy. Try again in a moment.".into()),
            Err(e) => self.message = Some(format!("Couldn't connect: {e}")),
        }
    }

    /// A tap on a network in the list.
    fn choose(&mut self, i: usize) {
        let Some(net) = self.nets.get(i).cloned() else { return };
        if self.busy() {
            return;
        }
        if self.state.is_joined(&net) {
            self.page = Page::Details { net };
            return;
        }
        match net.security {
            Security::Open => self.join(net, None, None),
            Security::Wpa2 => match self.saved.get(&net.ssid).copied().flatten() {
                Some(key) => self.join(net, Some(key), None),
                None => self.ask_password(net, String::new(), None),
            },
            Security::Other => {
                self.message = Some(format!("{} uses security this phone can't join yet", net.name()));
            }
        }
    }

    fn ask_password(&mut self, net: Network, text: String, error: Option<String>) {
        self.keyboard = Keyboard::default();
        self.page = Page::Password { net, text, show: false, error };
    }

    /// Follow the runtime's Wi-Fi state; true when something on screen changed.
    fn refresh(&mut self) -> bool {
        let s = wifi::state();
        let was = std::mem::replace(&mut self.state, s);
        let finished = self.state.busy.is_none();
        let mut changed = was != self.state;
        // Something else (the runtime rejoining a saved network) finished: show its scan.
        if finished && was.busy.is_some() && !self.scanning && self.joining.is_none() && !self.leaving {
            self.nets = wifi::networks();
            self.scroll = self.scroll.min(ui::max_scroll(self.nets.len()));
            changed = true;
        }
        if finished && self.scanning {
            self.scanning = false;
            match &self.state.last {
                Some((what, false, code)) if what == "scan" => {
                    self.message = Some(if *code == 3 { "Wi-Fi is unavailable right now".into() } else { "Scanning didn't work. Try again.".into() });
                }
                _ => {}
            }
            self.nets = wifi::networks();
            self.scroll = self.scroll.min(ui::max_scroll(self.nets.len()));
            println!("wifi: found {} networks", self.nets.len());
            changed = true;
        }
        if finished && self.leaving {
            self.leaving = false;
            self.message = Some(if self.state.connected == 0 { "Disconnected".into() } else { "Couldn't disconnect".into() });
            if matches!(self.page, Page::Details { .. }) {
                self.page = Page::List;
            }
            changed = true;
        }
        if finished && self.joining.is_some() {
            let j = self.joining.take().unwrap();
            let ok = self.state.connected != 0
                && matches!(&self.state.last, Some((what, true, _)) if what == "join");
            if ok {
                println!("wifi: joined {}", j.net.name());
                let key = if j.net.security == Security::Open { None } else { j.key };
                if j.net.security == Security::Open || key.is_some() {
                    self.saved.put_first(&j.net.ssid, key);
                    self.saved.store();
                }
                self.message = None;
                self.page = Page::List;
            } else {
                let code = self.state.last.as_ref().map_or(0, |l| l.2);
                println!("wifi: joining {} failed ({code})", j.net.name());
                let why = wifi::reason(code).to_string();
                // Only a refused handshake (52) means the password may be wrong. The
                // saved key stays saved either way: a timeout or a weak signal must
                // not lose it, and a new password replaces it only once it works.
                if code == 52 && j.typed.is_none() && self.saved.contains(&j.net.ssid) {
                    self.ask_password(j.net, String::new(), Some("The saved password didn't work. Enter it again.".into()));
                } else if let Some(text) = j.typed {
                    if let Page::Password { error, text: t, .. } = &mut self.page {
                        *error = Some(why);
                        *t = text;
                    }
                } else {
                    self.message = Some(why);
                }
            }
            changed = true;
        }
        changed
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if violet::HOME_BOX.contains(x, y) {
            return Some(Hit::Home);
        }
        match &self.page {
            Page::List => {
                if ui::SCAN.contains(x, y) {
                    return Some(Hit::Scan);
                }
                if (ui::LIST_TOP..ui::LIST_BOTTOM).contains(&y) {
                    return (0..self.nets.len()).find(|&i| ui::row_rect(i, self.scroll).contains(x, y)).map(Hit::Row);
                }
                None
            }
            Page::Password { .. } => {
                if ui::SHOW.contains(x, y) {
                    Some(Hit::Show)
                } else if ui::CONNECT.contains(x, y) {
                    Some(Hit::Connect)
                } else {
                    None
                }
            }
            Page::Details { net } => {
                let here = self.state.is_joined(net);
                if here && ui::DISCONNECT.contains(x, y) {
                    Some(Hit::Disconnect)
                } else if self.saved.contains(&net.ssid) && ui::FORGET.contains(x, y) {
                    Some(Hit::Forget)
                } else {
                    None
                }
            }
        }
    }

    fn activate(&mut self, hit: Hit) {
        match hit {
            Hit::Home => match self.page {
                Page::List => self.exit = true,
                _ => self.page = Page::List,
            },
            Hit::Scan => {
                self.message = None;
                self.scan();
            }
            Hit::Row(i) => self.choose(i),
            Hit::Show => {
                if let Page::Password { show, .. } = &mut self.page {
                    *show = !*show;
                }
            }
            Hit::Connect => {
                if self.busy() {
                    return;
                }
                let Page::Password { net, text, error, .. } = &mut self.page else { return };
                if !wifi::valid_passphrase(text) {
                    *error = Some("WPA2 passwords have 8 to 63 characters".into());
                    return;
                }
                *error = None;
                let (net, text) = (net.clone(), text.clone());
                let key = wifi::pmk(&net.ssid, &text);
                self.join(net, Some(key), Some(text));
            }
            Hit::Disconnect => {
                if self.busy() {
                    return;
                }
                match wifi::leave() {
                    Ok(()) => {
                        self.leaving = true;
                        self.state.busy = Some("leaving".into());
                    }
                    Err(e) => self.message = Some(format!("Couldn't disconnect: {e}")),
                }
            }
            Hit::Forget => {
                let Page::Details { net } = &self.page else { return };
                let net = net.clone();
                if self.saved.remove(&net.ssid) {
                    self.saved.store();
                }
                if self.state.is_joined(&net) && !self.busy() && wifi::leave().is_ok() {
                    self.leaving = true;
                    self.state.busy = Some("leaving".into());
                } else {
                    self.page = Page::List;
                }
            }
        }
    }

    fn type_key(&mut self, key: Key) {
        if self.joining.is_some() {
            return;
        }
        if let Page::Password { text, error, .. } = &mut self.page {
            if self.keyboard.press(key, text, 63) && key != Key::Shift && !matches!(key, Key::Page(_)) {
                *error = None;
            }
        }
    }

    pub fn touch(&mut self, kind: u16, x: i32, y: i32) {
        let on_keys = matches!(self.page, Page::Password { .. }) && y >= keyboard::TOP;
        match kind {
            TOUCH_DOWN => {
                self.touch = Some(Touch { start: (x, y), last: (x, y), scroll0: self.scroll, dragging: false });
                if on_keys {
                    self.keyboard.pressed = self.keyboard.key_at(x, y);
                    if self.keyboard.pressed == Some(Key::Back) {
                        self.held_back = Some(Instant::now());
                        self.type_key(Key::Back);
                    }
                } else {
                    self.pressed = self.hit(x, y);
                }
            }
            TOUCH_MOVE => {
                if let Some(t) = &mut self.touch {
                    t.last = (x, y);
                }
                let from_keys = matches!(self.page, Page::Password { .. })
                    && self.touch.as_ref().is_some_and(|t| t.start.1 >= keyboard::TOP);
                if from_keys {
                    let k = self.keyboard.key_at(x, y);
                    if k != self.keyboard.pressed {
                        self.held_back = None;
                    }
                    self.keyboard.pressed = k;
                    return;
                }
                let Some(t) = &mut self.touch else { return };
                if matches!(self.page, Page::List) && (y - t.start.1).abs() > 18 {
                    t.dragging = true;
                    self.pressed = None;
                    self.scroll = (t.scroll0 - (y - t.start.1)).clamp(0, ui::max_scroll(self.nets.len()));
                } else if !t.dragging && self.pressed != self.hit(x, y) {
                    self.pressed = None;
                }
            }
            TOUCH_UP => {
                self.held_back = None;
                if let Some(k) = self.keyboard.pressed.take() {
                    if k != Key::Back {
                        self.type_key(k);
                    }
                    self.touch = None;
                    return;
                }
                let Some(t) = self.touch.take() else { return };
                if let Some(h) = self.pressed.take() {
                    if !t.dragging && self.hit(t.last.0, t.last.1) == Some(h) {
                        self.activate(h);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod preview {
    //! WIFI_PREVIEW=dir cargo test -p settings writes each page as raw 720x1600 RGBA.
    use super::*;
    use crate::wifi_ui as ui;

    fn net(id: u32, name: &str, channel: u32, security: Security) -> Network {
        Network { id, channel, security, ssid: name.as_bytes().to_vec() }
    }

    #[test]
    fn preview() {
        let Ok(dir) = std::env::var("WIFI_PREVIEW") else { return };
        let fonts = Fonts::load();
        let nets = vec![
            net(7, "LouieDog", 6, Security::Wpa2),
            net(8, "Corner Cafe Guest", 1, Security::Open),
            net(9, "NETGEAR-5G", 149, Security::Wpa2),
            net(10, "Office", 36, Security::Other),
        ];
        let kb = Keyboard::default();
        let joined = wifi::State { connected: 7, ssid: b"LouieDog".to_vec(), ..Default::default() };
        let idle = wifi::State::default();
        let base = |page: &'static Page| ui::View {
            page, nets: &nets, state: &joined, connected_name: "LouieDog".into(), hosting: false, busy: None,
            joining: None, saved: vec![b"LouieDog".as_slice()], message: None, scroll: 0, pressed: None, keyboard: &kb, phase: 0.3,
        };
        let list: &'static Page = Box::leak(Box::new(Page::List));
        let pw: &'static Page = Box::leak(Box::new(Page::Password {
            net: nets[2].clone(), text: "hunter2!".into(), show: false, error: Some(wifi::reason(52).into()),
        }));
        let details: &'static Page = Box::leak(Box::new(Page::Details { net: nets[0].clone() }));
        let mut joining = base(list);
        joining.state = &idle;
        joining.joining = Some(9);
        let mut c = violet::Canvas::new(violet::W, violet::H);
        for (name, v) in [("list", base(list)), ("joining", joining), ("password", base(pw)), ("details", base(details))] {
            ui::draw(&mut c, &fonts, &v);
            let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
            std::fs::write(format!("{dir}/{name}.rgba"), bytes).unwrap();
        }
    }
}
