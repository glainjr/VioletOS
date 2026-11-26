//! Open files and the /dev/violet devices: the screen (a 720x1600 BGRA8888
//! double buffer an app maps and presents), touch and lifecycle events, and
//! a line-oriented system control file. Also the app list ("shell") the Open
//! Runtime draws when no app is in front.
use super::abi::*;
use super::sys::{self, FRAMEBUFFER, PAGE};
use super::{request, Runtime, Wait, MAX_APPS};
use crate::protocol::*;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

pub const WIDTH: usize = 720;
pub const HEIGHT: usize = 1600;
pub const STRIDE: usize = WIDTH * 4;
pub const BUFFER_BYTES: usize = (STRIDE * HEIGHT + PAGE - 1) & !(PAGE - 1);

pub enum Kind {
    File { path: String },
    Dir { path: String, entries: Vec<(String, bool)>, pos: usize },
    Screen,
    /// 48 kHz s16 PCM to the Audio service; `pending` holds a partial frame.
    Audio { channels: u32, pending: Vec<u8> },
    /// Frames from the Camera service; `seen` is the last frame returned.
    Camera { seen: u32 },
    /// The microphone stream (16 kHz s16 mono); `seen` counts samples read.
    Mic { seen: u32 },
    /// The Bluetooth controller's HCI (hci.rs): one H4 packet per read or write.
    Hci,
    Events,
    Sys { pending: Vec<u8> },
    Log,
    Null,
    Random,
    /// Interest: (fd, events, data). `reported` holds, for edge-triggered
    /// entries (EPOLLET), the readiness last reported, so only changes wake.
    Epoll { interest: Vec<(i32, u32, u64)>, reported: Vec<(i32, u32)> },
    EventFd { count: u64 },
    Pipe { buffer: Rc<RefCell<VecDeque<u8>>>, write: bool, peer_open: Rc<RefCell<u32>> },
    /// A TCP socket: its slot once connect was called, and the peer.
    Tcp { slot: Option<usize>, peer: (u32, u16) },
    /// A UDP socket: DNS questions to the runtime's resolver are answered here;
    /// anything else goes through a Radio slot, opened on first use. `replies`
    /// holds datagrams (source, payload) ready to read; `allowed` the
    /// destinations the Shield has passed.
    Udp { peer: Option<(u32, u16)>, replies: VecDeque<((u32, u16), Vec<u8>)>, slot: Option<usize>, allowed: Vec<(u32, u16)> },
}

pub struct OpenFile {
    pub kind: Kind,
    pub offset: u64,
    pub flags: u32,
    /// SO_RCVTIMEO in nanoseconds; 0 waits forever.
    pub recv_timeout: u64,
}
impl OpenFile {
    pub fn new(kind: Kind, flags: u32) -> Self {
        Self { kind, offset: 0, flags, recv_timeout: 0 }
    }
}
impl Drop for OpenFile {
    fn drop(&mut self) {
        if let Kind::Pipe { write: true, peer_open, .. } = &self.kind {
            *peer_open.borrow_mut() = 0;
        }
        if let Kind::Camera { .. } = self.kind {
            camera::want(false);
        }
        if let Kind::Mic { .. } = self.kind {
            mic::close();
        }
        if let Kind::Hci = self.kind {
            super::hci::close();
        }
        if let Kind::Tcp { slot: Some(slot), .. } | Kind::Udp { slot: Some(slot), .. } = self.kind {
            super::net::close(slot);
        }
    }
}

pub struct Surface {
    /// Pool pages: buffer 0 then buffer 1.
    pub pages: Vec<u32>,
    pub shown: u32,
    /// Rows and columns presented but not yet copied, because the panel was
    /// reading them (scanout.rs): x0, y0, x1, y1, and when it was first asked.
    pub pending: Option<([u32; 4], u64)>,
}
impl Surface {
    fn byte(&self, offset: usize) -> *mut u8 {
        let page = self.pages[offset / PAGE];
        (super::pool::page_va(page) + offset % PAGE) as *mut u8
    }
}

impl Shell {
    /// Arm for taps now unless a finger is already down (it must lift first).
    pub fn arm_with(&mut self, finger_down: bool) {
        self.armed = !finger_down;
        self.held = finger_down;
    }
}

#[derive(Default)]
pub struct Shell {
    pub dirty: bool,
    /// A touch counts only after the finger has been lifted since coming to the front.
    pub armed: bool,
    held: bool,
    down_at: Option<(i32, i32, u64)>,
    /// Where the finger was last reported, so a contact that has not moved
    /// does not wake the app again.
    last_xy: (i32, i32),
    pub message: String,
}

mod font {
    include!(env!("VIOLET_GLYPHS_RS"));
    pub fn tables() -> (&'static [u8], &'static [[u8; 5]]) {
        (ALPHABET, GLYPHS)
    }
}
fn glyph(c: u8) -> Option<&'static [u8; 5]> {
    let (alphabet, glyphs) = font::tables();
    let c = c.to_ascii_uppercase();
    alphabet.iter().position(|&a| a == c).map(|i| &glyphs[i])
}
pub const BG: u32 = 0xff191620;
pub const PANEL: u32 = 0xff302a39;
pub const INK: u32 = 0xffeeeaf8;
pub const ACCENT: u32 = 0xffd1b0fe;
pub const ALERT: u32 = 0xffff5a5f;
pub const OK_GREEN: u32 = 0xff6ee7a8;

fn fill(x: usize, y: usize, w: usize, h: usize, color: u32) {
    // Scanout is mapped cached, so plain stores (written back by the clean
    // below or by the caller) are as visible as volatile ones, and a row is
    // one fill instead of a store per pixel: the app list used to take long
    // enough to draw that a short tap during it went unseen.
    let (x0, x1) = (x.min(WIDTH), (x + w).min(WIDTH));
    for row in y..(y + h).min(HEIGHT) {
        let line = unsafe { core::slice::from_raw_parts_mut((FRAMEBUFFER as *mut u32).add(row * WIDTH + x0), x1 - x0) };
        line.fill(color);
    }
    if w == WIDTH && h == HEIGHT {
        sys::clean_framebuffer(0, HEIGHT); // a screen cleared outside draw_shell
    }
    super::scanout::mark_dirty(y as u32, (y + h).min(HEIGHT) as u32);
}
fn text(x: usize, y: usize, s: &str, scale: usize, color: u32) {
    for (n, c) in s.bytes().enumerate() {
        let Some(g) = glyph(c) else { continue };
        for (col, &bits) in g.iter().enumerate() {
            for bit in 0..7 {
                if bits & (1 << bit) != 0 {
                    fill(x + (6 * n + col) * scale, y + bit * scale, scale, scale, color);
                }
            }
        }
    }
}

/// The page and frame window shared with Camera (see services/camera/process.rs).
pub mod camera {
    const PAGE: *mut u32 = 0x0912_0000 as *mut u32;
    const WINDOW: usize = 0x0b00_0000;
    fn get(i: usize) -> u32 {
        unsafe { PAGE.add(i).read_volatile() }
    }
    /// Camera writes its magic when it starts; the page exists only with Camera.
    pub fn present(mapped: bool) -> bool {
        mapped && get(8) == 0x314d_4143
    }
    pub fn want(on: bool) {
        unsafe { PAGE.write_volatile(on as u32) }
    }
    pub fn frames() -> u32 {
        get(10)
    }
    /// The CameraFrame header (runtime/open/abi.rs) for the newest frame, its
    /// offset in the window and its length, or the header alone after a failure.
    pub fn newest() -> ([u32; 8], Option<(usize, usize)>) {
        let (state, error) = (get(9), get(16));
        let h = [0x3146_4356, get(10), get(12), get(13), get(14), get(15), get(17), state | error << 8];
        let frame = (state == 2).then(|| (WINDOW + get(11) as usize, (h[4] * h[3]) as usize));
        (h, frame)
    }
}

/// The microphone stream from Audio, in the pages after the PCM ring
/// (services/audio/process.rs, mod stream). Audio copies samples while
/// CAPTURE_WANTED is 1, which it is while any app has /dev/violet/mic open.
pub mod mic {
    use super::super::request;
    use crate::protocol::*;
    use core::sync::atomic::{AtomicU32, Ordering};
    const HEADER: *mut u32 = 0x0910_0000 as *mut u32;
    const RING: *const i16 = 0x0911_1000 as *const i16;
    pub const SAMPLES: u32 = 8 * 4096 / 2;
    static READERS: AtomicU32 = AtomicU32::new(0);
    pub fn written() -> u32 {
        unsafe { HEADER.add(17).read_volatile() }
    }
    /// Start (or keep) the microphone for one more reader.
    pub fn open() -> u32 {
        READERS.fetch_add(1, Ordering::Relaxed);
        unsafe { HEADER.add(16).write_volatile(1) };
        request(EXECUTE, MICROPHONE, MIC_LISTEN);
        written()
    }
    /// The microphone again after the app in front changed (it turns off then).
    pub fn resume() {
        if READERS.load(Ordering::Relaxed) > 0 {
            request(EXECUTE, MICROPHONE, MIC_LISTEN);
        }
    }
    pub fn close() {
        if READERS.fetch_sub(1, Ordering::Relaxed) == 1 {
            unsafe { HEADER.add(16).write_volatile(0) };
            request(EXECUTE, MICROPHONE, MIC_OFF);
        }
    }
    /// Samples after `seen`, up to `max`; an overrun skips to the newest second.
    pub fn take(seen: &mut u32, max: usize) -> alloc::vec::Vec<u8> {
        let w = written();
        core::sync::atomic::fence(Ordering::Acquire);
        if w.wrapping_sub(*seen) > SAMPLES {
            *seen = w.wrapping_sub(SAMPLES);
        }
        let n = (w.wrapping_sub(*seen) as usize).min(max);
        let mut v = alloc::vec::Vec::with_capacity(n * 2);
        for k in 0..n as u32 {
            let s = unsafe { RING.add((seen.wrapping_add(k) % SAMPLES) as usize).read_volatile() };
            v.extend_from_slice(&s.to_le_bytes());
        }
        *seen = seen.wrapping_add(n as u32);
        v
    }
}

/// The PCM ring shared with Audio (see services/audio/process.rs).
const PCM_HEADER: *mut u32 = 0x0910_0000 as *mut u32;
const PCM_RING: *mut [i16; 2] = 0x0910_1000 as *mut [i16; 2];
const PCM_FRAMES: u32 = 16 * 4096 / 4;
/// Audio writes its magic when it starts; the pages exist only with Audio.
pub fn audio_present(mapped: bool) -> bool {
    mapped && unsafe { PCM_HEADER.read_volatile() } == 0x314d_4350
}
fn pcm_space() -> u32 {
    let (written, played) = unsafe { (PCM_HEADER.add(2).read_volatile(), PCM_HEADER.add(3).read_volatile()) };
    PCM_FRAMES - written.wrapping_sub(played).min(PCM_FRAMES)
}
/// Queue whole frames from `data` (after any partial frame left over). Returns
/// the bytes taken, which may be fewer than given when the ring is full.
pub fn audio_write(channels: u32, pending: &mut Vec<u8>, data: &[u8]) -> usize {
    let frame_bytes = 2 * channels as usize;
    let room = pcm_space() as usize * frame_bytes;
    let take = data.len().min(room.saturating_sub(pending.len()));
    if take == 0 {
        return 0;
    }
    pending.extend_from_slice(&data[..take]);
    let whole = pending.len() / frame_bytes;
    let mut written = unsafe { PCM_HEADER.add(2).read_volatile() };
    for f in 0..whole {
        let at = f * frame_bytes;
        let l = i16::from_le_bytes([pending[at], pending[at + 1]]);
        let r = if channels == 2 { i16::from_le_bytes([pending[at + 2], pending[at + 3]]) } else { l };
        unsafe { PCM_RING.add((written % PCM_FRAMES) as usize).write_volatile([l, r]) };
        written = written.wrapping_add(1);
    }
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    unsafe { PCM_HEADER.add(2).write_volatile(written) };
    pending.drain(..whole * frame_bytes);
    take
}

pub const ROW_Y: usize = 260;
pub const ROW_H: usize = 80;
const ROWS: usize = (HOME_Y - 40 - ROW_Y) / ROW_H;
const BG_ROW: u32 = 0xff231d2e;
pub const HOME_Y: usize = 1380;

/// Row height for a list of `n` programs: 80 px while they fit, then smaller
/// (down to 40) so installed apps never fall off the end. QEMU tests use the same rule.
pub fn row_height(n: usize) -> usize {
    if n <= ROWS { ROW_H } else { ((HOME_Y - 40 - ROW_Y) / n).max(40) }
}

/// Shared settings (see "get" and "set" in /dev/violet/sys).
const SETTINGS: &str = "/data/settings";

fn setting_key(k: &str) -> bool {
    (1..=24).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Runtime {
    fn settings_text(&self) -> String {
        self.fs.contents(SETTINGS).map(|d| String::from_utf8_lossy(d).into_owned()).unwrap_or_default()
    }
    fn setting(&self, key: &str) -> Option<String> {
        self.settings_text().lines().find_map(|l| l.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| String::from(v)))
    }

    /// Titles from /apps/.home ("name<TAB>title" lines), for the apps that have one.
    fn titles(&self) -> Vec<(String, String)> {
        let Some(home) = self.fs.contents("/apps/.home") else { return Vec::new() };
        core::str::from_utf8(home)
            .unwrap_or("")
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(n, t)| (String::from(n), String::from(t)))
            .collect()
    }
    /// Programs under /apps: titled apps first, then the rest, each sorted.
    pub fn app_entries(&self) -> Vec<String> {
        let titled: Vec<String> = self.titles().into_iter().map(|(n, _)| n).collect();
        let others = self.fs.list("/apps").into_iter().map(|(n, _)| n)
            .filter(|n| !n.starts_with('.') && !titled.contains(n) && alloc::format!("/apps/{}", n) != super::HOME_APP);
        titled.iter().cloned().filter(|n| self.fs.stat(&alloc::format!("/apps/{}", n)).is_some())
            .chain(others)
            .map(|n| alloc::format!("/apps/{}", n))
            .collect()
    }

    /// Start the Home app, if there is one. It stays running; "home" shows it.
    pub fn start_home(&mut self) {
        if self.fs.stat(super::HOME_APP).is_none() {
            return;
        }
        match self.launch(super::HOME_APP, &[], None) {
            Ok(i) => {
                self.home = Some(i);
                self.set_foreground(Some(i));
            }
            Err(e) => crate::log!("could not start Home: {}", e),
        }
        // A paired voice agent starts behind Home, so it hears the user without
        // being opened first (its state is /data/agent; "peer" once paired).
        let paired = self.fs.contents("/data/agent").is_some_and(|d| d.split(|&b| b == b'\n').any(|l| l.starts_with(b"peer ")));
        if paired && self.fs.stat(super::appstore::INSTALLER).is_some() {
            match self.launch(super::appstore::INSTALLER, &[], None) {
                Ok(_) => crate::log!("started the agent in the background"),
                Err(e) => crate::log!("could not start the agent: {}", e),
            }
        }
    }
    /// Show the Home app when nothing else is in front (or the list without one).
    pub fn show_home(&mut self) {
        match self.home {
            Some(h) if self.apps[h].is_some() => {
                if self.foreground != Some(h) {
                    self.set_foreground(Some(h));
                    self.refresh(h);
                    self.lifecycle(h, LIFECYCLE_FOCUS_GAINED);
                }
            }
            _ => self.shell.dirty = true,
        }
    }

    pub fn draw_shell(&mut self) {
        self.shell.dirty = false;
        fill(0, 0, WIDTH, HEIGHT, BG);
        text(42, 60, "VIOLET OS", 3, ACCENT);
        text(42, 110, "OPEN APPS", 8, INK);
        let entries = self.app_entries();
        if entries.is_empty() {
            text(42, ROW_Y, "NO APPS IN /APPS", 4, INK);
        }
        let titles = self.titles();
        let h = row_height(entries.len());
        let fits = (HOME_Y - 40 - ROW_Y) / h;
        for (i, path) in entries.iter().take(fits).enumerate() {
            let y = ROW_Y + i * h;
            let name = path.trim_start_matches("/apps/");
            let title = titles.iter().find(|(n, _)| n == name).map(|(_, t)| t.to_ascii_uppercase());
            fill(42, y, WIDTH - 84, h - 12, if title.is_some() { PANEL } else { BG_ROW });
            let shown: String = title.as_deref().unwrap_or(name).chars().take(16).collect();
            let scale = if h == ROW_H { if title.is_some() { 5 } else { 4 } } else if h >= 52 { 4 } else { 3 };
            text(72, y + (h - 12 - 7 * scale) / 2, &shown, scale, INK);
            match self.apps.iter().flatten().find(|a| a.path == *path) {
                Some(a) if a.frozen.is_some() => {
                    fill(42, y, 10, h - 12, ALERT);
                    if h == ROW_H {
                        text(72, y + 52, "FROZEN BY VIOLET SHIELD", 2, ALERT);
                    }
                }
                Some(_) => text(WIDTH - 72 - 7 * 2 * 8, y + (h - 12 - 14) / 2, "RUNNING", 2, ACCENT),
                None => {}
            }
        }
        if !self.shell.message.is_empty() {
            let m: String = self.shell.message.chars().take(36).collect();
            text(42, HOME_Y - 50, &m, 3, ACCENT);
        }
        fill(42, HOME_Y, WIDTH - 84, 150, PANEL);
        text(78, HOME_Y + 54, "HOME", 7, INK);
        self.draw_shield_status();
        sys::clean_framebuffer(0, HEIGHT);
    }

    /// One line under the title: the companion link and the live Shields.
    pub fn draw_shield_status(&mut self) {
        fill(42, 190, WIDTH - 84, 40, BG);
        let linked = self.sentinel.online();
        let frozen = self.apps.iter().flatten().filter(|a| a.frozen.is_some()).count();
        let line = alloc::format!(
            "SHIELD {} - {} RULE{} - {} FROZEN",
            if linked { "LINKED" } else { "OFFLINE" },
            self.shield.active(),
            if self.shield.active() == 1 { "" } else { "S" },
            frozen
        );
        text(42, 196, &line, 3, if frozen > 0 { ALERT } else if linked { OK_GREEN } else { ACCENT });
    }

    fn shell_tap(&mut self, x: i32, y: i32) {
        let y = y as usize;
        if (HOME_Y..HOME_Y + 150).contains(&y) {
            if self.home.is_some() {
                self.show_home();
            } else {
                request(EXECUTE, OPEN, LAUNCHER);
            }
            return;
        }
        if y < ROW_Y || x < 42 || x > (WIDTH - 42) as i32 {
            return;
        }
        let entries = self.app_entries();
        let row = (y - ROW_Y) / row_height(entries.len());
        let Some(path) = entries.into_iter().nth(row) else { return };
        crate::log!("app list: tap at {},{} -> {}", x, y, path);
        if let Some(i) = (0..MAX_APPS).find(|&i| self.apps[i].as_ref().map_or(false, |a| a.path == path)) {
            self.set_foreground(Some(i));
            self.refresh(i);
            self.lifecycle(i, LIFECYCLE_FOCUS_GAINED);
            return;
        }
        match self.launch(&path, &[], None) {
            Ok(i) => {
                self.shell.message.clear();
                self.set_foreground(Some(i));
                fill(0, 0, WIDTH, HEIGHT, 0xff000000);
            }
            Err(e) => {
                crate::log!("could not start {}: {}", path, e);
                self.shell.message = e;
                self.shell.dirty = true;
            }
        }
    }

    /// A touch change from Runtime or the interrupt ring, in screen pixels.
    pub fn touch(&mut self, x: i32, y: i32, down: bool) {
        // A finger on the glass (the idle samples between touches do not count).
        if down {
            self.last_touch_ns = sys::now_ns();
        }
        if !self.shell.armed {
            self.shell.armed = !down;
            self.shell.held = down;
            return;
        }
        let Some(i) = self.foreground else {
            if down && !self.shell.held {
                self.shell_tap(x, y);
            }
            self.shell.held = down;
            return;
        };
        // Holding the top edge for a second returns to the app list.
        let now = sys::now_ns();
        if down && !self.shell.held {
            self.shell.down_at = Some((x, y, now));
        }
        // Touch is sampled continuously; only changes reach the app: a press,
        // a finger that moved, a release. (Every idle sample used to arrive
        // as another TOUCH_UP, waking the app in front ~250 times a second.)
        let kind = match (down, self.shell.held) {
            (true, false) => EVENT_TOUCH_DOWN,
            (true, true) if (x, y) != self.shell.last_xy => EVENT_TOUCH_MOVE,
            (true, true) => return,
            (false, true) => EVENT_TOUCH_UP,
            (false, false) => return,
        };
        self.shell.last_xy = (x, y);
        if let (false, Some((_, sy, t))) = (down, self.shell.down_at) {
            if sy < 48 && now - t > 1_000_000_000 {
                self.shell.held = false;
                self.shell.down_at = None;
                self.lifecycle(i, LIFECYCLE_FOCUS_LOST);
                self.foreground = None;
                self.show_home();
                return;
            }
        }
        self.shell.held = down;
        self.event(i, InputEvent { timestamp_us: now / 1000, kind, code: 0, x, y, value: 0 });
    }

    pub fn event(&mut self, i: usize, e: InputEvent) {
        if self.foreground == Some(i) && matches!(e.kind, EVENT_TOUCH_DOWN | EVENT_TOUCH_UP) {
            self.frames.input_delivered(sys::now_ns());
        }
        if self.foreground == Some(i) && e.kind == EVENT_LIFECYCLE && e.value == LIFECYCLE_FOCUS_GAINED {
            self.frames.focus_at = Some(sys::now_ns());
        }
        if let Some(app) = self.apps[i].as_mut() {
            if app.events.len() < 256 {
                app.events.push_back(e);
            }
        }
    }
    /// Make `next` the app in front (None: the app list). The app that was in
    /// front hears FOCUS_LOST, so it stops drawing and recording.
    pub fn set_foreground(&mut self, next: Option<usize>) {
        if self.foreground == next {
            return;
        }
        let now = sys::now_ns();
        if let Some(f) = self.foreground {
            if let Some(app) = self.apps[f].as_mut() {
                app.used_ns = now;
                self.lifecycle(f, LIFECYCLE_FOCUS_LOST);
            }
        }
        if let Some(app) = next.and_then(|n| self.apps[n].as_mut()) {
            app.used_ns = now;
        }
        self.foreground = next;
    }
    /// App memory in KiB: the pool left, and per running app its pages plus its
    /// screen surface, and its page tables.
    pub fn memory_report(&self) -> String {
        let mut out = alloc::format!("free={} total={}", self.pool.available() * 64, self.pool.total * 64);
        for a in self.apps.iter().flatten() {
            let (pages, tables) = a.space.usage();
            let surface = a.surface.as_ref().map_or(0, |s| s.pages.len());
            out += &alloc::format!(" {}={}+{}(t{})", a.name, pages * 64, surface * 64, tables);
        }
        out
    }
    fn holds_mic(&self, i: usize) -> bool {
        self.apps[i].as_ref().is_some_and(|a| a.fds.iter().flatten().any(|f| matches!(f.borrow().kind, Kind::Mic { .. })))
    }
    /// The app allowed to keep the microphone when it is not in front (the
    /// voice agent), while it holds it.
    pub fn background_listener(&self) -> Option<usize> {
        (0..MAX_APPS).find(|&n| self.apps[n].as_ref().is_some_and(|a| a.path == super::appstore::INSTALLER) && self.holds_mic(n))
    }
    /// Turns radios on or off as Settings › Radios says (radios.rs).
    pub fn apply_radios(&mut self, r: super::radios::Radios) {
        if r != self.radios {
            crate::log!("radios: wifi {} bluetooth {}", r.wifi as u8, r.bluetooth as u8);
        }
        self.radios = r;
        self.wifi.set_off(!r.wifi);
        super::hci::set_blocked(!r.bluetooth);
    }
    pub fn lifecycle(&mut self, i: usize, value: i32) {
        let now = sys::now_ns() / 1000;
        if value == LIFECYCLE_FOCUS_LOST && self.audio {
            // An app that leaves the front stops recording, as with any app,
            // unless the agent is listening from the background (Home shows it).
            if self.background_listener().is_none() {
                request(EXECUTE, MICROPHONE, MIC_OFF);
            } else if Some(i) == self.background_listener() {
                crate::log!("the agent keeps listening in the background");
            }
        }
        if value == LIFECYCLE_FOCUS_GAINED && self.audio {
            // An app back in front that still holds the microphone hears again.
            if self.holds_mic(i) {
                mic::resume();
            }
        }
        if value == LIFECYCLE_FOCUS_LOST && self.camera {
            // And its camera: an app may only watch while it is on screen.
            camera::want(false);
        }
        self.event(i, InputEvent { timestamp_us: now, kind: EVENT_LIFECYCLE, code: 0, x: 0, y: 0, value });
    }

    /// Show the buffer an app presented last, after it comes back to the front.
    pub fn refresh(&mut self, i: usize) {
        let shown = self.apps[i].as_ref().and_then(|a| a.surface.as_ref()).map(|s| s.shown);
        match shown {
            Some(b) => self.present(i, PresentRect { x: 0, y: 0, width: WIDTH as u32, height: HEIGHT as u32, buffer_index: b, flags: 0 }),
            None => fill(0, 0, WIDTH, HEIGHT, 0xff000000),
        }
    }

    pub fn present(&mut self, i: usize, r: PresentRect) {
        let on_screen = self.focused && self.foreground == Some(i);
        let Some(s) = self.apps[i].as_mut().and_then(|a| a.surface.as_mut()) else { return };
        if r.buffer_index > 1 {
            return;
        }
        if s.shown != r.buffer_index {
            s.pending = None; // rows of the other buffer no longer matter
        }
        s.shown = r.buffer_index;
        if !on_screen {
            s.pending = None;
            return;
        }
        let x0 = r.x.min(WIDTH as u32);
        let x1 = r.x.saturating_add(r.width).min(WIDTH as u32);
        let y0 = r.y.min(HEIGHT as u32);
        let y1 = r.y.saturating_add(r.height).min(HEIGHT as u32);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let now = sys::now_ns();
        s.pending = Some(match s.pending {
            Some((p, since)) => ([p[0].min(x0), p[1].min(y0), p[2].max(x1), p[3].max(y1)], since),
            None => ([x0, y0, x1, y1], now),
        });
        self.flush_present(i);
    }

    /// Copy app i's pending rows to scanout if the panel is clear of them
    /// (or has waited long enough). True when nothing is left pending.
    pub fn flush_present(&mut self, i: usize) -> bool {
        let on_screen = self.focused && self.foreground == Some(i);
        let Some(s) = self.apps[i].as_mut().and_then(|a| a.surface.as_mut()) else { return true };
        let Some(([x0, y0, x1, y1], since)) = s.pending else { return true };
        if !on_screen {
            s.pending = None;
            return true;
        }
        let start = sys::now_ns();
        if !self.beam.clear_of(y0, y1) && start - since < super::scanout::MAX_WAIT_NS {
            return false;
        }
        s.pending = None;
        let (x0, x1, y0, y1) = (x0 as usize, x1 as usize, y0 as usize, y1 as usize);
        let base = s.shown as usize * BUFFER_BYTES;
        let mut clean_ns = 0;
        // Bands of rows, each written back to memory before the next, so the
        // copy reaches DRAM well ahead of the panel.
        for band in (y0..y1).step_by(16) {
            let end_row = (band + 16).min(y1);
            for y in band..end_row {
                let mut off = base + y * STRIDE + x0 * 4;
                let end = base + y * STRIDE + x1 * 4;
                let mut dst = FRAMEBUFFER + y * STRIDE + x0 * 4;
                while off < end {
                    let n = (PAGE - off % PAGE).min(end - off);
                    sys::copy_words(dst as *mut u8, s.byte(off), n);
                    off += n;
                    dst += n;
                }
            }
            let t = sys::now_ns();
            sys::clean_framebuffer(band, end_row);
            clean_ns += sys::now_ns() - t;
        }
        super::scanout::mark_dirty(y0 as u32, y1 as u32);
        let end = sys::now_ns();
        self.frames.clean_ns += clean_ns;
        self.frames.waited.add(start - since);
        self.frames.presented(start, end, ((x1 - x0) * (y1 - y0)) as u64);
        true
    }

    /// Presents waiting for the panel to move on.
    pub fn flush_presents(&mut self) {
        for i in 0..MAX_APPS {
            if self.present_pending(i) {
                self.flush_present(i);
            }
        }
    }

    pub fn present_pending(&self, i: usize) -> bool {
        self.apps[i].as_ref().and_then(|a| a.surface.as_ref()).map_or(false, |s| s.pending.is_some())
    }

    /// The app's screen surface, created on first open.
    pub fn surface(&mut self, i: usize) -> Result<(), i32> {
        if self.apps[i].as_ref().unwrap().surface.is_some() {
            return Ok(());
        }
        let mut pages = Vec::new();
        for _ in 0..2 * BUFFER_BYTES / PAGE {
            if self.pool.available() == 0 && self.foreground == Some(i) {
                self.reclaim(None);
            }
            match self.pool.take() {
                Some(p) => pages.push(p),
                None => {
                    for p in pages {
                        self.pool.give_back(p);
                    }
                    return Err(super::syscall::ENOMEM);
                }
            }
        }
        self.apps[i].as_mut().unwrap().surface = Some(Surface { pages, shown: 0, pending: None });
        Ok(())
    }

    /// Map the surface into the app (mmap of /dev/violet/screen).
    pub fn map_surface(&mut self, i: usize, va: u32, len: u32, offset: u32) -> Result<(), i32> {
        self.surface(i)?;
        let app = self.apps[i].as_mut().unwrap();
        let pages = app.surface.as_ref().unwrap().pages.clone();
        let first = offset as usize / PAGE;
        let count = (len as usize + PAGE - 1) / PAGE;
        if first + count > pages.len() {
            return Err(super::syscall::EINVAL);
        }
        for n in 0..count {
            let at = va + (n * PAGE) as u32;
            app.space.map(&mut self.k, &self.pool, at, pages[first + n], false)?;
        }
        Ok(())
    }

    /// Whether a wait can finish now, and with what result.
    pub fn ready(&mut self, i: usize, wait: &Wait) -> Option<i32> {
        match wait {
            Wait::Futex { .. } | Wait::Sleep | Wait::Frozen => None,
            Wait::Present { result } => (!self.present_pending(i)).then_some(*result),
            Wait::Vsync { after, ptr } => {
                if self.beam.frame <= *after {
                    return None;
                }
                let (ptr, info) = (*ptr, self.vsync_info());
                if ptr != 0 {
                    let app = self.apps[i].as_mut().unwrap();
                    app.space.write(&mut self.k, &mut self.pool, ptr, &info);
                }
                Some(0)
            }
            Wait::Audio { fd, buf, len } => {
                let r = self.write_audio(i, *fd, *buf, *len);
                (r != 0).then_some(r)
            }
            Wait::Connect { fd } => self.connect_result(i, *fd),
            Wait::Send { fd, buf, len } => {
                let r = self.write(i, *fd, *buf, *len, None);
                (r != super::syscall::EAGAIN).then_some(r)
            }
            Wait::Read { fd, buf, len } => {
                let r = self.read_now(i, *fd, *buf, *len);
                (r != super::syscall::EAGAIN).then_some(r)
            }
            Wait::RecvFrom { fd, buf, len, name, name_len } => {
                let r = self.recv_from(i, *fd, *buf, *len, *name, *name_len);
                (r != super::syscall::EAGAIN).then_some(r)
            }
            Wait::Poll { fds, ptr, time64: _ } => {
                let mut n = 0;
                let mut out = Vec::new();
                for &(fd, events) in fds {
                    let rev = self.poll_mask(i, fd) & (events as u32 | 0x18);
                    if rev != 0 {
                        n += 1;
                    }
                    out.push(rev as i16);
                }
                if n == 0 {
                    return None;
                }
                let app = self.apps[i].as_mut().unwrap();
                for (k, rev) in out.iter().enumerate() {
                    app.space.write(&mut self.k, &mut self.pool, ptr + 8 * k as u32 + 6, &rev.to_le_bytes());
                }
                Some(n)
            }
            Wait::Epoll { epfd, events, max } => {
                let n = self.epoll_collect(i, *epfd, *events, *max);
                (n > 0).then_some(n)
            }
        }
    }

    /// VsyncInfo for SCREEN_IOC_WAIT_VSYNC: sequence, period in µs, and when
    /// the frame started in µs of the monotonic clock.
    pub fn vsync_info(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&(self.beam.frame as u32).to_le_bytes());
        b[4..8].copy_from_slice(&self.beam.period_us().to_le_bytes());
        b[8..16].copy_from_slice(&(self.beam.frame_ns / 1000).to_le_bytes());
        b
    }

    /// poll(2) bits for a descriptor: POLLIN 1, POLLOUT 4, POLLHUP 0x10, POLLNVAL 0x20.
    pub fn poll_mask(&mut self, i: usize, fd: i32) -> u32 {
        let app = self.apps[i].as_ref().unwrap();
        let Some(Some(f)) = app.fds.get(fd as usize) else { return 0x20 };
        let f = f.borrow();
        match &f.kind {
            Kind::Events => {
                if app.events.is_empty() { 4 } else { 5 }
            }
            Kind::EventFd { count } => 4 | (*count > 0) as u32,
            Kind::Pipe { buffer, write: false, peer_open } => {
                let b = buffer.borrow();
                (!b.is_empty()) as u32 | if *peer_open.borrow() == 0 { 0x11 } else { 0 }
            }
            Kind::Pipe { write: true, .. } => 4,
            Kind::Audio { .. } => if pcm_space() > 0 { 4 } else { 0 },
            Kind::Camera { seen } => (camera::frames() != *seen && self.focused && self.foreground == Some(i)) as u32,
            Kind::Mic { seen } => (mic::written() != *seen) as u32,
            Kind::Hci => super::hci::readable() as u32 | if super::hci::space() >= 1028 { 4 } else { 0 },
            Kind::Udp { replies, slot, .. } => 4 | (!replies.is_empty() || slot.is_some_and(|s| self.net.rx_ready(s) > 0)) as u32,
            Kind::Tcp { slot: None, .. } => 4,
            Kind::Tcp { slot: Some(slot), .. } => {
                let net = &self.net;
                match net.state(*slot).0 {
                    super::net::CONNECTING => 0,
                    super::net::OPEN => (net.rx_ready(*slot) > 0) as u32 | if net.tx_space(*slot) > 0 { 4 } else { 0 },
                    super::net::PEER_CLOSED => 1 | 0x10 | if net.rx_ready(*slot) > 0 { 0 } else { 0 } | 4,
                    _ => 1 | 4 | 8 | 0x10,
                }
            }
            Kind::Epoll { .. } => 0,
            Kind::Sys { pending } => 4 | (!pending.is_empty()) as u32,
            _ => 5,
        }
    }

    pub fn epoll_collect(&mut self, i: usize, epfd: i32, events: u32, max: u32) -> i32 {
        let interest = {
            let app = self.apps[i].as_ref().unwrap();
            let Some(Some(f)) = app.fds.get(epfd as usize) else { return super::syscall::EBADF };
            let f = f.borrow();
            match &f.kind {
                Kind::Epoll { interest, .. } => interest.clone(),
                _ => return super::syscall::EINVAL,
            }
        };
        const EPOLLET: u32 = 1 << 31;
        let mut n = 0u32;
        let mut edges: Vec<(i32, u32)> = Vec::new();
        for (fd, mask, data) in interest {
            if n >= max {
                break;
            }
            let ready = self.poll_mask(i, fd) & (mask | 0x18);
            let mut rev = ready;
            if mask & EPOLLET != 0 {
                // Edge-triggered writability: an idle socket stays writable, and
                // Go's poller would wake for it forever. EPOLLOUT is reported
                // once, and again after a write would block (epoll_rearm). Input
                // stays level-triggered: readers drain until EAGAIN.
                const EPOLLOUT: u32 = 4;
                let last = self.epoll_reported(i, epfd, fd);
                rev = ready & !(last & EPOLLOUT);
                edges.push((fd, ready & EPOLLOUT));
            }
            if rev == 0 {
                continue;
            }
            // struct epoll_event on ARM EABI: u32 events, 4 bytes padding, u64 data.
            let mut bytes = [0u8; 16];
            bytes[..4].copy_from_slice(&rev.to_le_bytes());
            bytes[8..].copy_from_slice(&data.to_le_bytes());
            let app = self.apps[i].as_mut().unwrap();
            app.space.write(&mut self.k, &mut self.pool, events + 16 * n, &bytes);
            n += 1;
        }
        if !edges.is_empty() {
            if let Some(Some(f)) = self.apps[i].as_ref().unwrap().fds.get(epfd as usize) {
                if let Kind::Epoll { reported, .. } = &mut f.borrow_mut().kind {
                    for (fd, ready) in edges {
                        match reported.iter_mut().find(|r| r.0 == fd) {
                            Some(r) => r.1 = ready,
                            None => reported.push((fd, ready)),
                        }
                    }
                }
            }
        }
        n as i32
    }

    /// A write to fd would block: report its writability again when it returns.
    pub fn epoll_rearm(&mut self, i: usize, fd: i32) {
        for f in self.apps[i].as_ref().unwrap().fds.iter().flatten() {
            if let Ok(mut f) = f.try_borrow_mut() {
                if let Kind::Epoll { reported, .. } = &mut f.kind {
                    reported.retain(|r| r.0 != fd);
                }
            }
        }
    }

    fn epoll_reported(&self, i: usize, epfd: i32, fd: i32) -> u32 {
        let Some(Some(f)) = self.apps[i].as_ref().unwrap().fds.get(epfd as usize) else { return 0 };
        match &f.borrow().kind {
            Kind::Epoll { reported, .. } => reported.iter().find(|r| r.0 == fd).map_or(0, |r| r.1),
            _ => 0,
        }
    }

    /// Read without blocking; EAGAIN when nothing is available yet.
    pub fn read_now(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> i32 {
        use super::syscall::*;
        let f = match self.apps[i].as_ref().unwrap().fds.get(fd as usize) {
            Some(Some(f)) => f.clone(),
            _ => return EBADF,
        };
        let mut f = f.borrow_mut();
        let offset = f.offset;
        let data: Vec<u8> = match &mut f.kind {
            Kind::File { path } => {
                let Some(c) = self.fs.contents(path) else { return EIO };
                let start = (offset as usize).min(c.len());
                let end = (start + len as usize).min(c.len());
                c[start..end].to_vec()
            }
            Kind::Dir { .. } => return EISDIR,
            Kind::Null | Kind::Log => Vec::new(),
            Kind::Random => {
                let mut v = Vec::with_capacity(len as usize);
                let mut x = sys::counter() | 1;
                while v.len() < len as usize {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    v.push((x >> 24) as u8);
                }
                v
            }
            Kind::Events => {
                let app = self.apps[i].as_mut().unwrap();
                let mut v = Vec::new();
                while v.len() + 24 <= len as usize {
                    let Some(e) = app.events.pop_front() else { break };
                    let raw: [u8; 24] = unsafe { core::mem::transmute(e) };
                    v.extend_from_slice(&raw);
                }
                if v.is_empty() {
                    return if len < 24 { EINVAL } else { EAGAIN };
                }
                v
            }
            Kind::Sys { pending } => {
                if pending.is_empty() {
                    return 0;
                }
                let n = pending.len().min(len as usize);
                pending.drain(..n).collect()
            }
            Kind::Tcp { slot: None, .. } => return -107, // ENOTCONN
            Kind::Tcp { slot: Some(slot), .. } => {
                let slot = *slot;
                let (state, error) = self.net.state(slot);
                let got = self.net.recv(slot, len as usize);
                if got.is_empty() {
                    return match state {
                        super::net::PEER_CLOSED => 0,
                        super::net::FAILED => -error.max(1),
                        super::net::CONNECTING => EAGAIN,
                        _ => EAGAIN,
                    };
                }
                got
            }
            Kind::Udp { replies, slot, .. } => {
                if let Some(d) = slot.and_then(|s| self.net.recv_datagram(s)) {
                    replies.push_back(d);
                }
                let Some((from, mut r)) = replies.pop_front() else { return EAGAIN };
                r.truncate(len as usize);
                self.net.last_from = from;
                r
            }
            Kind::EventFd { count } => {
                if *count == 0 {
                    return EAGAIN;
                }
                let v = count.to_le_bytes().to_vec();
                *count = 0;
                v
            }
            Kind::Pipe { buffer, write: false, peer_open } => {
                let mut b = buffer.borrow_mut();
                if b.is_empty() {
                    return if *peer_open.borrow() == 0 { 0 } else { EAGAIN };
                }
                let n = b.len().min(len as usize);
                b.drain(..n).collect()
            }
            Kind::Hci => {
                if !super::hci::up() {
                    return ENETDOWN;
                }
                match super::hci::take(len as usize) {
                    Some(v) => v,
                    None => return EAGAIN,
                }
            }
            Kind::Mic { seen } => {
                let v = mic::take(seen, len as usize / 2);
                if v.is_empty() {
                    return if len < 2 { EINVAL } else { EAGAIN };
                }
                v
            }
            Kind::Camera { seen } => {
                // Only the app on screen sees frames (not while the phone
                // sleeps or another screen is up); reading keeps the camera on.
                if !self.focused || self.foreground != Some(i) || len < 32 {
                    return if len < 32 { EINVAL } else { EAGAIN };
                }
                camera::want(true);
                let (mut h, frame) = camera::newest();
                // A failure is reported once, as a header alone.
                let next = match h[7] & 0xff {
                    2 => h[1],
                    3 => u32::MAX,
                    _ => return EAGAIN,
                };
                if next == *seen {
                    return EAGAIN;
                }
                *seen = next;
                let mut n = 0;
                let app = self.apps[i].as_mut().unwrap();
                if let Some((at, bytes)) = frame {
                    n = bytes.min(len as usize - 32);
                    let pixels = unsafe { core::slice::from_raw_parts(at as *const u8, n) };
                    if !app.space.write(&mut self.k, &mut self.pool, buf + 32, pixels) {
                        return EFAULT;
                    }
                    // A frame that Camera overwrote while it was copied is marked torn.
                    if camera::frames().wrapping_sub(h[1]) > 1 {
                        h[7] |= 1 << 31;
                    }
                }
                let raw: [u8; 32] = unsafe { core::mem::transmute(h) };
                if !app.space.write(&mut self.k, &mut self.pool, buf, &raw) {
                    return EFAULT;
                }
                return 32 + n as i32;
            }
            _ => return EBADF,
        };
        if matches!(f.kind, Kind::File { .. }) {
            f.offset += data.len() as u64;
        }
        drop(f);
        let app = self.apps[i].as_mut().unwrap();
        if !app.space.write(&mut self.k, &mut self.pool, buf, &data) {
            return EFAULT;
        }
        data.len() as i32
    }

    /// Write PCM; 0 means the ring is full and nothing was taken.
    pub fn write_audio(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> i32 {
        use super::syscall::*;
        let f = match self.apps[i].as_ref().unwrap().fds.get(fd as usize) {
            Some(Some(f)) => f.clone(),
            _ => return EBADF,
        };
        let data = {
            let app = self.apps[i].as_mut().unwrap();
            let mut v = alloc::vec![0u8; len.min(64 << 10) as usize];
            if !app.space.read(&mut self.k, &mut self.pool, buf, &mut v) {
                return EFAULT;
            }
            v
        };
        let mut f = f.borrow_mut();
        let Kind::Audio { channels, pending } = &mut f.kind else { return EBADF };
        audio_write(*channels, pending, &data) as i32
    }

    /// A command written to /dev/violet/sys. Answers are read back from it.
    pub fn system(&mut self, i: usize, line: &str) -> String {
        let mut words = line.split_whitespace();
        match words.next() {
            // Back to the Home app (or, without one, the classic launcher).
            Some("home") => {
                if self.home.is_some() && self.home != Some(i) {
                    if self.foreground == Some(i) {
                        self.lifecycle(i, LIFECYCLE_FOCUS_LOST);
                    }
                    self.foreground = None;
                    self.show_home();
                    return String::from("ok\n");
                }
                if self.foreground == Some(i) {
                    self.foreground = None;
                    self.shell.dirty = true;
                }
                request(EXECUTE, OPEN, LAUNCHER);
                String::from("ok\n")
            }
            // The list of every program in /apps.
            Some("apps") => {
                self.set_foreground(None);
                self.shell.dirty = true;
                String::from("ok\n")
            }
            // Bluetooth: whether the controller is on, and whether an app has its HCI open.
            Some("bluetooth") => alloc::format!("on={} open={}\n", super::hci::up() as u32, super::hci::is_open() as u32),
            // Settings › Radios (runtime/open/radios.rs).
            Some("radios") => match words.next() {
                None => self.radios.report(),
                Some(_) if self.apps[i].as_ref().map_or(true, |a| a.path != super::wifi::SETTINGS_APP) => {
                    String::from("error denied: only Settings changes the radios\n")
                }
                Some(what) => match words.next().and_then(|state| self.radios.change(what, state)) {
                    Some(next) => {
                        self.apply_radios(next);
                        match self.fs.writable(super::radios::SAVED, true, true, 0) {
                            Some(f) => {
                                f.extend_from_slice(next.text().as_bytes());
                                self.radios.report()
                            }
                            None => String::from("error couldn't save the choice\n"),
                        }
                    }
                    None => String::from("error usage: radios [wifi|bluetooth on|off | all off]\n"),
                },
            },
            // Phone Internet (runtime/open/wifi.rs).
            Some("relay") => {
                let path = self.apps[i].as_ref().map_or(String::new(), |a| a.path.clone());
                self.wifi.relay(&path, &mut words)
            }
            // Bring an app to the front, starting it if it is not running.
            Some("open") => match words.next() {
                Some(path) => {
                    if let Some(n) = (0..MAX_APPS).find(|&n| self.apps[n].as_ref().map_or(false, |a| a.path == path)) {
                        if self.foreground != Some(n) {
                            self.set_foreground(Some(n));
                            self.refresh(n);
                            self.lifecycle(n, LIFECYCLE_FOCUS_GAINED);
                        }
                        return String::from("ok\n");
                    }
                    match self.launch(path, &[], Some(i)) {
                        Ok(n) => {
                            self.set_foreground(Some(n));
                            fill(0, 0, WIDTH, HEIGHT, 0xff000000);
                            String::from("ok\n")
                        }
                        Err(e) => alloc::format!("error {}\n", e),
                    }
                }
                None => String::from("error usage: open PATH\n"),
            },
            // Frame-pipeline measurements since the last `frames` (frames.rs).
            Some("frames") => alloc::format!("{} {}", super::touch::describe(), self.frames.report(sys::now_ns())),
            // `bench tap X Y N`: N synthetic presses on the app in front;
            // `bench drag X Y DY N`: N drags of DY pixels over 250 ms each.
            Some("bench") => {
                let verb = words.next();
                let (drag, release) = (verb == Some("drag"), verb == Some("press"));
                let mut n = words.map(|w| w.parse::<i32>().unwrap_or(-1));
                let (x, y) = (n.next().unwrap_or(-1), n.next().unwrap_or(-1));
                let dy = if drag { n.next().unwrap_or(0) } else { 0 };
                match n.next() {
                    Some(count) if x >= 0 && y >= 0 && count > 0 && drag == (dy != 0) => {
                        let now = sys::now_ns();
                        self.frames.report(now);
                        self.frames.bench = Some(super::frames::Bench { x, y, dy, step: 0, left: count as u32, next_ns: now + 300_000_000, pressed: false, release });
                        String::from("ok\n")
                    }
                    _ => String::from("error usage: bench tap X Y N | bench press X Y N | bench drag X Y DY N\n"),
                }
            }
            // The programs running now, as paths.
            // Installed apps (runtime/open/appstore.rs): "install NAME TITLE" keeps
            // the staged program as /apps/NAME across restarts and lists it under
            // TITLE; "uninstall NAME" removes it; "installed" lists them. Only the
            // agent (/apps/agent), which takes installs from the paired Mac, may
            // install them; the agent and Settings may remove them.
            Some(verb @ ("install" | "uninstall")) => {
                let path = self.apps[i].as_ref().map_or(String::new(), |a| a.path.clone());
                let allowed = path == super::appstore::INSTALLER || (verb == "uninstall" && path == super::wifi::SETTINGS_APP);
                if !allowed {
                    return String::from("error only the agent installs apps\n");
                }
                let Some(name) = words.next().filter(|n| super::appstore::valid_name(n)) else {
                    return alloc::format!("error usage: {} NAME{}\n", verb, if verb == "install" { " TITLE" } else { "" });
                };
                let installed = alloc::format!("/apps/{}", name);
                let image = self.fs.stat(&installed).is_some() && !self.fs.from_layer(&installed);
                if image || installed == super::appstore::INSTALLER {
                    return String::from("error a built-in app has that name\n");
                }
                let title: Vec<&str> = words.collect();
                let now = (self.clock(0) / 1_000_000_000) as u32;
                let _lowered = sys::Lowered::new();
                let running = self.apps.iter().position(|app| app.as_ref().is_some_and(|a| a.path == installed));
                let r = if verb == "install" {
                    self.store.install(&mut self.fs, name, &title.join(" "), now).map(|bytes| {
                        // A successful install invalidates only this app's old process.
                        if let Some(n) = running {
                            self.reap(n);
                        }
                        alloc::format!("ok {}\n", bytes)
                    })
                } else {
                    // A running copy ends first.
                    if let Some(n) = running {
                        self.reap(n);
                    }
                    self.store.remove(&mut self.fs, name).map(|_| String::from("ok\n"))
                };
                self.shell.dirty = true;
                match r {
                    Ok(s) => {
                        crate::log!("{} {}", verb, name);
                        s
                    }
                    Err(e) => alloc::format!("error {}\n", e),
                }
            }
            Some("installed") => self.store.list(),
            // App memory: the pool left, and what each running app holds (its pages,
            // its screen surface), in KiB. "@sys memory" over USB.
            Some("memory") => alloc::format!("{}\n", self.memory_report()),
            Some("running") => {
                let paths: Vec<&str> = self.apps.iter().flatten().map(|a| a.path.as_str()).collect();
                alloc::format!("{}\n", paths.join(" "))
            }
            // Shared settings ("get KEY", "set KEY VALUE"), kept by the runtime in
            // /data/settings, so apps can share a preference (Weather's city, which
            // Home shows) without opening each other's private /data files.
            Some("get") => match words.next().filter(|k| setting_key(k)) {
                Some(key) => alloc::format!("{}\n", self.setting(key).unwrap_or_default()),
                None => String::from("error\n"),
            },
            Some("set") => match (words.next().filter(|k| setting_key(k)), words.next()) {
                (Some(key), Some(value)) if value.len() <= 64 && words.next().is_none() => {
                    let mut lines: Vec<String> = self.settings_text().lines()
                        .filter(|l| l.split_once('=').map_or(false, |(k, _)| k != key)).map(String::from).collect();
                    lines.push(alloc::format!("{}={}", key, value));
                    let text = lines.join("\n") + "\n";
                    match self.fs.writable(SETTINGS, true, true, 0) {
                        Some(f) if text.len() <= 4096 => {
                            f.extend_from_slice(text.as_bytes());
                            String::from("ok\n")
                        }
                        _ => String::from("error\n"),
                    }
                }
                _ => String::from("error\n"),
            },
            Some("shield-agent") if cfg!(moto_board)
                && self.apps[i].as_ref().is_some_and(|app| app.path == super::appstore::INSTALLER) =>
            {
                match words.next() {
                    Some("pull") => self.sentinel.agent_pull(sys::now_ns(), false),
                    Some("start") => self.sentinel.agent_pull(sys::now_ns(), true),
                    Some("push") if words.next().is_some_and(|text| self.sentinel.agent_push(text)) => String::from("ok"),
                    _ => String::from("error"),
                }
            }
            // Violet Shield: the companion link, live rules and frozen apps, for Home.
            Some("shield") => {
                let frozen: Vec<&str> = self.apps.iter().flatten().filter(|a| a.frozen.is_some()).map(|a| a.name.as_str()).collect();
                alloc::format!(
                    "linked={} rules={} frozen={} warnings={}\n",
                    self.sentinel.online() as u32,
                    self.shield.active(),
                    if frozen.is_empty() { String::from("-") } else { frozen.join(",") },
                    self.sentinel.warnings
                )
            }
            Some("status") => {
                let power = request(GET, POWER, 0);
                let volume = request(GET, VOLUME, 0);
                // listening=1 while the agent holds the microphone (in front or not).
                alloc::format!(
                    "battery={} volume={} headphones={} focused={} listening={} emulator={}\n",
                    if power.op == OK { power.value as i32 } else { -1 },
                    if volume.op == OK { volume.value as i32 } else { -1 },
                    (volume.op == OK && volume.arg & AUDIO_HEADPHONES != 0) as u32,
                    (self.focused && self.foreground == Some(i)) as u32,
                    self.background_listener().is_some() as u32,
                    (!cfg!(moto_board)) as u32
                )
            }
            Some("volume") if line.split_whitespace().nth(1).map_or(false, |w| w == "up" || w == "down") => {
                let step = if words.next() == Some("up") { VOLUME_UP } else { VOLUME_DOWN };
                let r = request(EXECUTE, VOLUME, step);
                if r.op == OK { alloc::format!("volume={}\n", r.value) } else { String::from("error\n") }
            }
            Some("volume") => {
                let level = words.next().and_then(|w| w.parse::<u32>().ok()).unwrap_or(u32::MAX);
                let r = request(EXECUTE, VOLUME, level);
                if r.op == OK { alloc::format!("volume={}\n", r.value) } else { String::from("error\n") }
            }
            // Audio state in one line: output, volume, playback and microphone.
            Some("audio") => {
                let volume = request(GET, VOLUME, 0);
                let playback = request(GET, PLAYBACK, 0);
                let mic = request(GET, MICROPHONE, 0);
                if volume.op != OK {
                    return String::from("error no audio\n");
                }
                let f = volume.arg;
                let outcome = match playback.arg & PLAYBACK_OUTCOME {
                    PLAYBACK_PLAYED => "played",
                    PLAYBACK_STOPPED => "stopped",
                    PLAYBACK_NO_OUTPUT => "no-output",
                    _ => "ready",
                };
                let mode = match mic.arg & MIC_MODE {
                    MIC_LISTEN => "listen",
                    MIC_RECORD => "record",
                    _ => "off",
                };
                alloc::format!(
                    "volume={} max={} headphones={} speaker={} headphone_out={} mic={} playing={} outcome={} mic_mode={} mic_level={} recording_ms={}\n",
                    volume.value, VOLUME_MAX, (f & AUDIO_HEADPHONES != 0) as u32, (f & AUDIO_SPEAKER_OUT != 0) as u32,
                    (f & AUDIO_HEADPHONE_OUT != 0) as u32, (f & AUDIO_MIC != 0) as u32,
                    if playback.op == OK { sound_name(playback.value) } else { "none" }, outcome, mode,
                    if mic.op == OK { mic.value } else { 0 },
                    if mic.op == OK { (mic.arg >> 16) * 10 } else { 0 }
                )
            }
            // "sound chime", "sound tone 440 1500", "sound recording", "sound stop".
            Some("sound") => {
                let name = words.next().unwrap_or("");
                let sound = match name {
                    "stop" => SOUND_STOP,
                    "click" => SOUND_CLICK,
                    "chime" => SOUND_CHIME,
                    "alert" => SOUND_ALERT,
                    "tone" => SOUND_TONE,
                    "recording" => SOUND_RECORDING,
                    _ => return String::from("error usage: sound stop|click|chime|alert|tone HZ MS|recording\n"),
                };
                let hz = words.next().and_then(|w| w.parse::<u32>().ok()).unwrap_or(440).min(0xffff);
                let ms = words.next().and_then(|w| w.parse::<u32>().ok()).unwrap_or(1500).min(0xffff);
                let arg = if sound == SOUND_TONE { hz | ms << 16 } else { 0 };
                let r = super::request_with(EXECUTE, PLAYBACK, sound, arg);
                if r.op == OK { String::from("ok\n") } else { alloc::format!("error {}\n", r.op) }
            }
            // "mic record" (up to four seconds), "mic listen", "mic off".
            Some("mic") => {
                let mode = match words.next() {
                    Some("record") => MIC_RECORD,
                    Some("listen") => MIC_LISTEN,
                    Some("off") => MIC_OFF,
                    _ => return String::from("error usage: mic record|listen|off\n"),
                };
                let r = request(EXECUTE, MICROPHONE, mode);
                if r.op == OK { String::from("ok\n") } else { alloc::format!("error {}\n", r.op) }
            }
            Some("launch") => match words.next() {
                Some(path) => match self.launch(path, &[], Some(i)) {
                    Ok(n) => {
                        self.set_foreground(Some(n));
                        String::from("ok\n")
                    }
                    Err(e) => alloc::format!("error {}\n", e),
                },
                None => String::from("error usage: launch PATH\n"),
            },
            // Wi-Fi: scan, list, join and leave (runtime/open/wifi.rs).
            Some("wifi") => {
                let path = self.apps[i].as_ref().map_or(String::new(), |a| a.path.clone());
                self.wifi.command(&path, &mut words)
            }
            Some("log") => {
                let rest: Vec<&str> = words.collect();
                crate::log!("{}", rest.join(" "));
                String::from("ok\n")
            }
            _ => String::from("error unknown command\n"),
        }
    }
}

#[allow(dead_code)]
fn _size_check() {
    let _: [u8; 24] = unsafe { core::mem::transmute(InputEvent::default()) };
}

fn sound_name(sound: u32) -> &'static str {
    match sound {
        SOUND_CLICK => "click",
        SOUND_CHIME => "chime",
        SOUND_ALERT => "alert",
        SOUND_TONE => "tone",
        SOUND_RECORDING => "recording",
        SOUND_STREAM => "stream",
        _ => "none",
    }
}
