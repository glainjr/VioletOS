//! Settings: Wi-Fi, Bluetooth, the phone link (Violet Shield and Phone
//! Internet), the clock's city, the apps Violet Agent built, and About.
mod pages;
mod wifi;
mod wifi_ui;

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use pages::{Hit, Section, Status, MENU};
use violet::device::{TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP};
use violet::{Canvas, Fonts, Screen, CITIES};

/// Settings' saved Phone Internet choice, which the runtime restores after a
/// restart (runtime/open/wifi.rs): "on CODE" or "off CODE".
const RELAY_SAVED: &str = "/data/relay";

fn new_code() -> String {
    let mut bytes = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    }
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn status() -> Status {
    let sh = violet::fields(&violet::sys("shield"));
    let bt = violet::fields(&violet::sys("bluetooth"));
    let st = violet::fields(&violet::sys("status"));
    let radios = violet::fields(&violet::sys("radios"));
    Status {
        wifi: wifi::App::summary(),
        bt_on: bt.get("on").is_some_and(|v| v == "1"),
        bt_open: bt.get("open").is_some_and(|v| v == "1"),
        shield_known: sh.contains_key("linked"),
        shield_linked: sh.get("linked").is_some_and(|v| v == "1"),
        shield_rules: sh.get("rules").and_then(|v| v.parse().ok()).unwrap_or(0),
        frozen: sh.get("frozen").filter(|v| *v != "-").map(|v| v.split(',').map(String::from).collect()).unwrap_or_default(),
        relay: String::new(),
        relay_failure: 0,
        // A percentage; the handset's Power reports millivolts for now, so hide anything else.
        battery: st.get("battery").and_then(|v| v.parse().ok()).filter(|b| (0..=100).contains(b)).unwrap_or(-1),
        volume: st.get("volume").and_then(|v| v.parse().ok()).unwrap_or(-1),
        city: violet::sys("get time.city").parse().ok().filter(|&n: &usize| n < CITIES.len()).unwrap_or(0),
        installed: pages::parse_installed(&violet::sys("installed")),
        radio_wifi: radios.get("wifi").is_none_or(|v| v != "off"),
        radio_bluetooth: radios.get("bluetooth").is_none_or(|v| v != "off"),
    }
}

fn relay_state(s: &mut Status) {
    let r = violet::fields(&violet::sys("relay"));
    s.relay = r.get("state").cloned().unwrap_or_else(|| "unavailable".into());
    s.relay_failure = r.get("failure").and_then(|v| v.parse().ok()).unwrap_or(0);
}

struct App {
    section: Section,
    status: Status,
    wifi: wifi::App,
    code: String,
    relay_on: bool,
    pressed: Option<Hit>,
    down: Option<(i32, i32)>,
    confirm: Option<usize>,
    note: String,
}

impl App {
    fn view(&self) -> pages::View<'_> {
        pages::View {
            section: self.section,
            status: &self.status,
            code: &self.code,
            relay_on: self.relay_on,
            pressed: self.pressed,
            confirm: self.confirm,
            note: &self.note,
        }
    }

    fn save_relay(&mut self) {
        let text = format!("{} {}\n", if self.relay_on { "on" } else { "off" }, self.code);
        if std::fs::write(RELAY_SAVED, text).is_err() {
            self.note = "Couldn't save the code".into();
        }
    }

    fn go(&mut self, section: Section) {
        println!("settings: {:?}", section);
        self.section = section;
        self.confirm = None;
        self.note.clear();
        self.refresh();
        if section == Section::Wifi {
            self.wifi.enter();
        }
    }

    fn refresh(&mut self) {
        self.status = status();
        if self.section == Section::PhoneLink || self.section == Section::Menu {
            relay_state(&mut self.status);
        }
    }

    fn activate(&mut self, hit: Hit) {
        match hit {
            // Saved settings and Phone Internet are managed by the runtime.
            // Release this screen's memory when the user returns to Home.
            Hit::Back if self.section == Section::Menu => std::process::exit(0),
            Hit::Back => self.go(Section::Menu),
            Hit::Row(i) => self.go(MENU[i].0),
            Hit::Relay => {
                let answer = if self.relay_on { violet::sys("relay off") } else { violet::sys(&format!("relay on {}", self.code)) };
                if answer == "ok" {
                    self.relay_on = !self.relay_on;
                    self.note.clear();
                    self.save_relay();
                    println!("settings: phone internet {}", if self.relay_on { "on" } else { "off" });
                } else {
                    self.note = "The radio is busy. Try again in a moment.".into();
                }
                relay_state(&mut self.status);
            }
            Hit::NewCode => {
                self.code = new_code();
                self.save_relay();
            }
            Hit::City(i) => {
                if violet::sys(&format!("set time.city {i}")) == "ok" {
                    self.status.city = i;
                    println!("settings: time city {}", CITIES[i]);
                }
            }
            Hit::Delete(i) if self.confirm == Some(i) => {
                self.confirm = None;
                let Some(a) = self.status.installed.get(i).cloned() else { return };
                self.uninstall(&[a]);
            }
            Hit::DeleteAll if self.confirm == Some(usize::MAX) => {
                self.confirm = None;
                let all = self.status.installed.clone();
                self.uninstall(&all);
            }
            Hit::Delete(i) => self.confirm = Some(i),
            Hit::Info(i) => self.confirm = if self.confirm == Some(i) { None } else { Some(i) },
            Hit::Switch(i) => {
                let r = pages::RADIOS[i];
                let key = if r == pages::Radio::Wifi { "wifi" } else { "bluetooth" };
                let to = if self.status.radio_on(r) { "off" } else { "on" };
                self.radios(&format!("radios {key} {to}"));
            }
            Hit::AllOff => self.radios("radios all off"),
            Hit::DeleteAll => self.confirm = Some(usize::MAX),
        }
    }

    /// A Settings › Radios change; the runtime answers with the radios' new state.
    fn radios(&mut self, command: &str) {
        let answer = violet::sys(command);
        println!("settings: {command} -> {answer}");
        self.note = if answer.starts_with("error") { "Couldn't change the radio".into() } else { String::new() };
        self.refresh();
    }

    fn uninstall(&mut self, apps: &[pages::Installed]) {
        self.note.clear();
        for a in apps {
            let answer = violet::sys(&format!("uninstall {}", a.name));
            println!("settings: uninstall {} -> {}", a.name, answer);
            if answer != "ok" {
                self.note = format!("Couldn't delete {}", a.title);
            }
        }
        self.refresh();
    }

    /// True when the screen needs drawing.
    fn touch(&mut self, kind: u16, x: i32, y: i32) -> bool {
        if self.section == Section::Wifi {
            self.wifi.touch(kind, x, y);
            if self.wifi.exit {
                self.go(Section::Menu);
            }
            return true;
        }
        match kind {
            TOUCH_DOWN => {
                self.down = Some((x, y));
                self.pressed = pages::hit(&self.view(), x, y);
                if self.pressed.is_none() && !matches!(self.confirm, None) {
                    self.confirm = None;
                }
            }
            TOUCH_MOVE => {
                self.down = Some((x, y));
                if self.pressed.is_some() && pages::hit(&self.view(), x, y) != self.pressed {
                    self.pressed = None;
                }
            }
            TOUCH_UP => {
                if let Some(h) = self.pressed.take() {
                    self.activate(h);
                }
            }
            _ => return false,
        }
        true
    }
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    let saved = std::fs::read_to_string(RELAY_SAVED).unwrap_or_default();
    let mut words = saved.split_whitespace();
    let relay_on = words.next() == Some("on");
    let code = words.next().filter(|c| c.len() == 12).map(String::from).unwrap_or_else(new_code);
    let mut app = App {
        section: Section::Menu,
        status: status(),
        wifi: wifi::App::new(),
        code,
        relay_on,
        pressed: None,
        down: None,
        confirm: None,
        note: String::new(),
    };
    if saved.is_empty() {
        app.save_relay();
    }
    relay_state(&mut app.status);
    println!("settings: ready, city={}", CITIES[app.status.city]);
    let draw = |c: &mut Canvas, app: &App| {
        if app.section == Section::Wifi {
            app.wifi.draw(c, &fonts);
        } else {
            pages::draw(c, &fonts, &app.view());
        }
    };
    let mut dirty = true;
    let mut last_poll = Instant::now();
    let mut wifi_poll = Instant::now();
    loop {
        if dirty {
            draw(&mut c, &app);
            screen.show_all(&c);
            dirty = false;
        }
        let animating = app.section == Section::Wifi && app.wifi.animating();
        let timeout = if animating { Duration::from_millis(120) } else { Duration::from_secs(1) };
        let mut next = match events.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Handle everything that queued up, then draw once.
        while let Some(e) = next {
            if e.focus_gained() {
                app.pressed = None;
                let section = app.section;
                app.go(section);
                dirty = true;
            } else if matches!(e.kind, TOUCH_DOWN | TOUCH_MOVE | TOUCH_UP) {
                dirty |= app.touch(e.kind, e.x, e.y);
            }
            next = events.try_recv().ok();
        }
        if app.section == Section::Wifi {
            dirty |= app.wifi.tick(&mut wifi_poll);
        } else if last_poll.elapsed() >= Duration::from_secs(if app.section == Section::PhoneLink { 1 } else { 3 }) {
            last_poll = Instant::now();
            let before = app.status.clone();
            app.refresh();
            dirty |= before != app.status;
        }
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    fn status() -> Status {
        Status {
            wifi: "LouieDog".into(), bt_on: true, bt_open: false, shield_known: true, shield_linked: true, shield_rules: 3,
            frozen: vec![], relay: "off".into(), relay_failure: 0, battery: 82, volume: 8, city: 1,
            installed: pages::parse_installed("scorekeeper\t412345\tScorekeeper\ntips\t300000\tTip Calculator\n"),
            radio_wifi: true, radio_bluetooth: false,
        }
    }

    #[test]
    fn installed_lines() {
        let a = pages::parse_installed("dice\t2048\tDice\nbad\nraw\t10\t\npython\t5000000\tPython\tpackage\n.free\t9000\t\n");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].title, "Dice");
        assert_eq!(a[1].title, "raw");
    }

    /// SETTINGS_PREVIEW=dir cargo test -p settings writes each page as raw 720x1600 RGBA.
    #[test]
    fn preview() {
        let Ok(dir) = std::env::var("SETTINGS_PREVIEW") else { return };
        let fonts = Fonts::load();
        let s = status();
        let mut c = Canvas::new(violet::W, violet::H);
        for (name, section) in [("menu", Section::Menu), ("bluetooth", Section::Bluetooth), ("radios", Section::Radios), ("phone", Section::PhoneLink),
                                ("time", Section::Time), ("apps", Section::AgentApps), ("about", Section::About)] {
            let v = pages::View { section, status: &s, code: "3FA94C0B17DE", relay_on: false, pressed: None, confirm: Some(1), note: "" };
            pages::draw(&mut c, &fonts, &v);
            let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
            std::fs::write(format!("{dir}/{name}.rgba"), bytes).unwrap();
        }
    }
}
