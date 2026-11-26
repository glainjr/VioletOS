//! Open Runtime ("Runtime v2"): runs unmodified Linux ARM EABI programs on
//! seL4. Each app is an seL4 address space whose threads have this process as
//! their fault handler. A Linux `svc #0` is an unknown syscall to seL4, so it
//! arrives here as a fault message; the reply carries the result. Page faults
//! map memory on demand. Built-in apps keep using Runtime, which knows this
//! process as client 9 (OPEN_APPS).
pub mod abi;
pub mod appstore;
pub mod data;
#[cfg(moto_board)]
pub mod display;
pub mod device;
pub mod frames;
pub mod fs;
pub mod hci;
pub mod heap;
pub mod inflate;
pub mod net;
pub mod scanout;
pub mod sockets;
pub mod touch;
pub mod policy;
pub mod shield;
pub mod guard;
#[path = "../../services/sentinel/mod.rs"]
pub mod sentinel;
pub mod pool;
pub mod radios;
#[path = "../../system/supervisor/procmgr.rs"]
pub mod procmgr;
pub mod syscall;
pub mod sys;
pub mod wifi;

use crate::protocol::*;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use policy::{Action, AppId, OpenPolicy, Policy};
use shield::{EventKind, ShieldPolicy, TelemetryEvent};
use pool::{Kernel, Pool};
use procmgr::Space;
use sys::Object;

/// Apps that may run at once; each gets a slice of kernel memory for its objects.
#[cfg(moto_board)]
pub const MAX_APPS: usize = 6;
#[cfg(not(moto_board))]
pub const MAX_APPS: usize = 8;
/// The Home app: started at boot and shown whenever no other app is in front.
pub const HOME_APP: &str = "/apps/home";
const APP_KERNEL_BITS: u32 = 19; // 512 KiB of kernel objects per app
const APP_CNODE_BITS: u32 = 8;
/// Pages of the pool kept for the runtime's own heap.
const HEAP_PAGES: usize = 768; // 48 MiB: the runtime, pushed apps and file data

pub struct Log;
impl Log {
    fn page() -> *mut u32 {
        sys::LOG_PAGE as *mut u32
    }
    pub fn init() {
        unsafe {
            Self::page().add(1).write_volatile(0);
            Self::page().write_volatile(0x474f4c4f); // "OLOG"
        }
    }
    /// Append to the ring: word 0 magic, word 1 bytes written in total, data from byte 8.
    pub fn write(bytes: &[u8]) {
        const SIZE: u32 = 4096 - 8;
        unsafe {
            let head = Self::page().add(1).read_volatile();
            let data = (sys::LOG_PAGE + 8) as *mut u8;
            for (i, b) in bytes.iter().enumerate() {
                data.add(((head + i as u32) % SIZE) as usize).write_volatile(*b);
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Self::page().add(1).write_volatile(head.wrapping_add(bytes.len() as u32));
        }
    }
}
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let s = alloc::format!("[open] {}\n", alloc::format!($($arg)*));
        $crate::process::Log::write(s.as_bytes());
    }};
}

#[derive(Clone)]
pub enum Wait {
    Futex { app: usize, addr: u32 },
    Sleep,
    Poll { fds: Vec<(i32, i16)>, ptr: u32, time64: bool },
    Epoll { epfd: i32, events: u32, max: u32 },
    Read { fd: i32, buf: u32, len: u32 },
    /// recvfrom/recvmsg: a read whose source address goes to `name`.
    RecvFrom { fd: i32, buf: u32, len: u32, name: u32, name_len: u32 },
    /// A blocking write to /dev/violet/audio waiting for room in the ring.
    Audio { fd: i32, buf: u32, len: u32 },
    /// A blocking connect, or a send waiting for room in the socket's ring.
    Connect { fd: i32 },
    Send { fd: i32, buf: u32, len: u32 },
    /// Held by a Violet Shield freeze; answered with EACCES when the app thaws.
    Frozen,
    /// A present waiting for the panel to scan past its rows (scanout.rs);
    /// answers the write with `result`.
    Present { result: i32 },
    /// SCREEN_IOC_WAIT_VSYNC: until a frame after `after` starts; VsyncInfo to `ptr`.
    Vsync { after: u64, ptr: u32 },
}

/// The outcome of the Shield check at the choke point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Allow,
    Deny,
    /// The app was frozen; the calling thread waits until it thaws.
    Frozen,
    /// The app was ended.
    Killed,
}

pub struct Frozen {
    pub until_ns: u64,
    pub shield_id: u32,
    /// Threads this freeze suspended (the rest were already blocked in a call).
    pub suspended: Vec<usize>,
}

/// Bytes an app has sent on a connection, reported as SOCK_TX.
pub struct TxStat {
    pub pid: u32,
    pub index: usize,
    pub host: String,
    pub port: u16,
    pub pending: u32,
    pub reported: u64,
    pub last_ns: u64,
}

pub struct Thread {
    pub tid: u32,
    pub tcb: u32,
    pub reply: Option<u32>,
    pub wait: Option<Wait>,
    pub deadline: Option<u64>,
    /// The fault message to answer with when it wakes: r0..r7 and FaultIP.
    pub saved: [u32; 9],
    pub clear_tid: u32,
    pub alive: bool,
    pub slot: u32,
    /// Its scheduling context (MCS), or 0.
    pub sc: u32,
}

pub type FileRef = Rc<RefCell<device::OpenFile>>;

pub struct App {
    pub pid: u32,
    pub name: String,
    pub path: String,
    pub space: Space,
    pub cnode: u32,
    pub threads: Vec<Thread>,
    pub fds: Vec<Option<FileRef>>,
    pub cwd: String,
    pub stub_page: u32,
    pub events: alloc::collections::VecDeque<abi::InputEvent>,
    pub surface: Option<device::Surface>,
    pub exit_code: Option<i32>,
    /// SHA-256 of the program file (the script, for interpreted programs).
    pub digest: [u8; 32],
    /// Not part of the boot image (written or pushed since boot).
    pub new_hash: bool,
    pub frozen: Option<Frozen>,
    /// When it was last in front (ns), to close the least recently used first.
    pub used_ns: u64,
}

/// Room the pool keeps for the app in front: a launch closes background apps
/// until this much is free (32 MiB).
const RESERVE_PAGES: usize = 512;

pub struct Runtime {
    pub k: Kernel,
    pub pool: Pool,
    pub fs: fs::Fs,
    /// Installed apps kept on disk (appstore.rs).
    pub store: appstore::Store,
    pub apps: [Option<App>; MAX_APPS],
    untypeds: Vec<u32>,
    pub policy: OpenPolicy,
    /// Runtime has put this process in front.
    pub focused: bool,
    /// The app on screen while focused; None shows the app list.
    pub foreground: Option<usize>,
    /// The running Home app, if /apps/home exists.
    pub home: Option<usize>,
    home_tried: bool,
    pub shell: device::Shell,
    next_pid: u32,
    pub random: u64,
    contact: Option<u32>,
    last_poll: u64,
    /// Loop statistics, logged every 30 s while apps run.
    pub stats: Stats,
    usb_serial: u32,
    /// Audio is installed, so its PCM ring is mapped at 0x09100000.
    pub audio: bool,
    /// The Camera service's page and frames are mapped.
    pub camera: bool,
    pub net: net::Net,
    pub shield: ShieldPolicy,
    pub sentinel: sentinel::Sentinel,
    pub last_touch_ns: u64,
    pub tx_stats: Vec<Option<TxStat>>,
    last_sentinel: u64,
    /// The "wifi" commands' running Radio requests.
    pub wifi: wifi::Wifi,
    /// Settings › Radios (radios.rs), and whether /data/radios was applied yet.
    pub radios: radios::Radios,
    radios_loaded: bool,
    /// Present cost, input-to-frame latency and touch polling (frames.rs).
    pub frames: frames::Frames,
    /// The panel's scan line and frame count (scanout.rs).
    pub beam: scanout::Beam,
    /// Reports Touch reads on its interrupt (touch.rs, handset).
    pub ring: touch::Ring,
    /// The app whose threads run in the foreground tier (MCS), as last applied.
    tiered: Option<usize>,
}

/// A request to Runtime, waiting for its ticket like the app SDK does.
pub fn request(op: u32, key: u32, value: u32) -> Message {
    request_with(op, key, value, 0)
}
/// As `request`, with the argument word (a tone's pitch and length).
pub fn request_with(op: u32, key: u32, value: u32, arg: u32) -> Message {
    let r = Message::new(op, key, value).with_arg(arg).call(sys::SLOT_RUNTIME);
    if r.op != OK {
        return Message::error(r.op);
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        sys::pause();
    }
}

fn badge(app: usize, thread: usize) -> u32 {
    ((app as u32) << 12) | (thread as u32 + 1)
}

impl Runtime {
    fn new() -> Result<Runtime, i32> {
        let mut k = Kernel::new();
        // Each step fails cleanly if the supervisor could not provide its part.
        sys::retype(sys::SLOT_KERNEL, Object::Endpoint, 0, sys::SLOT_FAULT_EP).map_err(|e| { log!("no kernel memory"); e })?;
        let (mut pool, _) = Pool::new(&mut k, HEAP_PAGES).map_err(|e| { log!("no app memory pool"); e })?;
        let mut untypeds = Vec::new();
        for _ in 0..MAX_APPS {
            untypeds.push(k.object(sys::SLOT_KERNEL, Object::Untyped, APP_KERNEL_BITS)?);
        }
        let mut image: &'static [u8] =
            unsafe { core::slice::from_raw_parts(sys::INITFS as *const u8, sys::INITFS_BYTES) };
        // The handset's image is compressed: "VGZ1", the gzip length, the gzip.
        if &image[..4] == b"VGZ1" {
            let packed_len = u32::from_le_bytes(image[4..8].try_into().unwrap()) as usize;
            let packed = &image[8..(8 + packed_len).min(image.len())];
            let size = inflate::gzip_size(packed).unwrap_or(0);
            let base = pool.take_top((size + sys::PAGE - 1) / sys::PAGE);
            let Some(base) = base else {
                log!("no room to unpack the {} byte file system image", size);
                return Err(-1);
            };
            let dest = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, size) };
            let start = sys::now_ns();
            match inflate::gunzip(packed, dest) {
                Ok(n) => {
                    log!("unpacked the file system image: {} KiB in {} ms", n >> 10, (sys::now_ns() - start) / 1_000_000);
                    image = &dest[..n];
                }
                Err(e) => {
                    log!("file system image: {}", e);
                    image = &[];
                }
            }
        }
        let mut fs = fs::Fs::new(image);
        data::load(&mut fs);
        let store = appstore::Store::load(&mut fs);
        Ok(Runtime {
            k,
            pool,
            fs,
            store,
            apps: core::array::from_fn(|_| None),
            home: None,
            home_tried: false,
            untypeds,
            policy: OpenPolicy,
            focused: false,
            foreground: None,
            shell: device::Shell::default(),
            next_pid: 100,
            random: sys::counter() ^ 0x9e3779b97f4a7c15,
            contact: None,
            last_poll: 0,
            stats: Stats::default(),
            audio: false,
            camera: false,
            // Radio (handset) or the emulator's host relay serves the socket pages.
            net: net::Net::new(true),
            shield: ShieldPolicy::new(),
            sentinel: {
                let mut id = [0u8; 16];
                let (a, b) = (sys::counter() ^ 0x5bd1e995, sys::counter().rotate_left(17) ^ 0x9e3779b97f4a7c15);
                id[..8].copy_from_slice(&a.to_le_bytes());
                id[8..].copy_from_slice(&b.to_le_bytes());
                sentinel::Sentinel::new(crate::companion_key::COMPANION_KEY, id)
            },
            last_touch_ns: 0,
            tx_stats: (0..net::SLOTS).map(|_| None).collect(),
            last_sentinel: 0,
            wifi: wifi::Wifi::default(),
            radios: radios::Radios::default(),
            radios_loaded: false,
            frames: frames::Frames::default(),
            beam: scanout::Beam::default(),
            ring: touch::Ring::default(),
            tiered: None,
            // Requests made before this boot are not repeated.
            usb_serial: if cfg!(moto_board) { unsafe { (0x0900_1000 as *const u32).read_volatile() } } else { 0 },
        })
    }

    pub fn next_pid_bump(&mut self) -> u32 {
        self.next_pid += 1;
        self.next_pid - 1
    }

    pub fn next_random(&mut self) -> u64 {
        // xorshift64*, reseeded from the counter's low bits on every call.
        self.random ^= sys::counter() & 0xffff;
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        self.random.wrapping_mul(0x2545f4914f6cdd1d)
    }

    /// Start a program from the file system. Scripts name their interpreter
    /// on a "#!" line; a ".py" file without one runs under /bin/python3.
    pub fn launch(&mut self, path: &str, args: &[String], caller: Option<usize>) -> Result<usize, String> {
        // Loading a program can take a while; Audio and Radio keep running.
        let _lowered = sys::Lowered::new();
        let st = self.fs.stat(path).ok_or_else(|| alloc::format!("{} not found", path))?;
        let mut program = String::from(path);
        let mut argv: Vec<String> = Vec::new();
        if st.mode & fs::S_IFMT == fs::S_IFDIR {
            for main in ["main", "main.py"] {
                let p = alloc::format!("{}/{}", path, main);
                if self.fs.stat(&p).is_some() {
                    program = p;
                    break;
                }
            }
        }
        let image = self.fs.contents(&program).ok_or("not a file")?;
        let digest = sentinel::sha256(image);
        let new_hash = self.fs.from_layer(&program);
        let measured = program.clone();
        if image.starts_with(b"#!") {
            let line_end = image.iter().position(|&b| b == b'\n').unwrap_or(image.len());
            let line = core::str::from_utf8(&image[2..line_end]).map_err(|_| "bad #! line")?;
            let mut words = line.split_whitespace();
            let interp = String::from(words.next().ok_or("empty #! line")?);
            argv.push(interp.clone());
            argv.extend(words.map(String::from));
            argv.push(program.clone());
            program = interp;
        } else if program.ends_with(".py") {
            argv.push(String::from("/bin/python3"));
            argv.push(program.clone());
            program = String::from("/bin/python3");
        } else {
            argv.push(program.clone());
        }
        argv.extend(args.iter().cloned());
        let image = self.fs.contents(&program).ok_or_else(|| alloc::format!("{} not found", program))?;
        // The image is part of the file system, which outlives the copy below.
        let image: &'static [u8] = unsafe { core::slice::from_raw_parts(image.as_ptr(), image.len()) };

        // A new foreground app needs both an app slot and room for its startup.
        while (self.pool.available() < RESERVE_PAGES || self.apps.iter().all(Option::is_some))
            && self.reclaim(caller)
        {}
        let index = self.apps.iter().position(|a| a.is_none()).ok_or("too many apps running")?;
        let mut random = [0u8; 16];
        let (a, b) = (self.next_random(), self.next_random());
        random[..8].copy_from_slice(&a.to_le_bytes());
        random[8..].copy_from_slice(&b.to_le_bytes());
        let untyped = self.untypeds[index];
        let k = &mut self.k;
        let mut space = Space::new(k, untyped).map_err(|e| alloc::format!("address space: {}", e))?;
        let cnode = match space.object(k, Object::CNode, APP_CNODE_BITS) {
            Ok(c) => c,
            Err(e) => {
                self.release_objects(index, core::mem::take(&mut space.object_slots));
                return Err(alloc::format!("cnode: {}", e));
            }
        };
        let name = String::from(path.rsplit('/').next().unwrap_or(path));
        let cwd = String::from(match path.rfind('/') {
            Some(0) | None => "/",
            Some(i) => &path[..i],
        });
        let env: Vec<String> = [
            "HOME=/data",
            "PATH=/bin",
            "TMPDIR=/tmp",
            "LANG=C.UTF-8",
            "TERM=dumb",
            "PYTHONHOME=/",
            "PYTHONDONTWRITEBYTECODE=1",
            "PYTHONUNBUFFERED=1",
            "GODEBUG=asyncpreemptoff=1",
            "VIOLET=1",
        ]
        .iter()
        .map(|s| String::from(*s))
        .collect();
        let loaded = match procmgr::load(&mut space, k, &mut self.pool, image, &argv, &env, random) {
            Ok(l) => l,
            Err(e) => {
                space.destroy(k, &mut self.pool);
                self.release_objects(index, core::mem::take(&mut space.object_slots));
                return Err(alloc::format!("{}: {}", program, e));
            }
        };
        // ARM-mode trampolines: `ldr pc, [pc, #-4]` then the target, per thread.
        space.add(procmgr::STUBS, procmgr::STUBS + sys::PAGE as u32, procmgr::PROT_READ | procmgr::PROT_EXEC);
        let Some(stub_page) = space.resolve(k, &mut self.pool, procmgr::STUBS) else {
            space.destroy(k, &mut self.pool);
            self.release_objects(index, core::mem::take(&mut space.object_slots));
            return Err(String::from("out of memory"));
        };
        let pid = self.next_pid;
        self.next_pid += 1;
        let mut app = App {
            pid,
            name,
            path: String::from(path),
            space,
            cnode,
            threads: Vec::new(),
            fds: Vec::new(),
            cwd,
            stub_page: stub_page as u32,
            events: Default::default(),
            surface: None,
            exit_code: None,
            digest,
            new_hash,
            frozen: None,
            used_ns: sys::now_ns(),
        };
        for _ in 0..3 {
            app.fds.push(Some(Rc::new(RefCell::new(device::OpenFile::new(device::Kind::Log, 2)))));
        }
        let regs = sys::Regs { sp: loaded.sp, ..Default::default() };
        self.apps[index] = Some(app);
        if let Err(e) = self.spawn(index, pid, regs, loaded.entry) {
            self.reap(index);
            return Err(alloc::format!("thread: {}", e));
        }
        log!("started {} as pid {} ({} KiB free)", program, pid, self.pool.available() * 64);
        let app_name = self.apps[index].as_ref().unwrap().name.clone();
        self.sentinel.note_exec(pid, &app_name, &digest);
        let _ = measured;
        let line = argv.join(" ");
        match self.gate(index, &Action::SpawnApp { path: &line }) {
            Gate::Deny | Gate::Killed => {
                if let Some(app) = self.apps[index].as_mut() {
                    app.exit_code = Some(126);
                }
                self.reap(index);
                return Err(String::from("blocked by Violet Shield"));
            }
            _ => {}
        }
        Ok(index)
    }

    /// Create a thread in app `index` that starts at `target` (bit 0 selects
    /// Thumb) with `regs`. seL4 clears the Thumb bit in a written CPSR, so the
    /// thread starts at its ARM-mode trampoline, which jumps there.
    pub fn spawn(&mut self, index: usize, tid: u32, mut regs: sys::Regs, target: u32) -> Result<usize, i32> {
        let app = self.apps[index].as_mut().unwrap();
        let slot = app.threads.len();
        // The app's CNode holds one badged fault endpoint per thread.
        if slot >= (1 << APP_CNODE_BITS) - 2 {
            return Err(-11);
        }
        let k = &mut self.k;
        let tcb = app.space.object(k, Object::Tcb, 0)?;
        let ep_slot = slot as u32 + 1;
        sys::mint_into(app.cnode, ep_slot, APP_CNODE_BITS, sys::SLOT_FAULT_EP, badge(index, slot))?;
        sys::tcb_configure(tcb, ep_slot, app.cnode, APP_CNODE_BITS, app.space.pd)?;
        // MCS: its own scheduling context, from the app's kernel memory, and
        // the fault endpoint named in this CSpace.
        let mut sc = 0;
        if sys::mcs() {
            sc = app.space.object(k, Object::SchedContext, sys::SCHED_CONTEXT_BITS)?;
            let fault = k.slot();
            sys::mint_into(sys::SLOT_CNODE, fault, sys::CNODE_BITS, sys::SLOT_FAULT_EP, badge(index, slot))?;
            let tier = if self.tiered == Some(index) { sys::tier_foreground() } else { sys::tier_background(index) };
            let r = sys::tcb_schedule(tcb, sc, fault, tier);
            sys::delete(fault);
            k.release(fault);
            r?;
        } else if sys::cores() > 1 {
            // Without MCS: just the core.
            let tier = if self.tiered == Some(index) { sys::tier_foreground() } else { sys::tier_background(index) };
            sys::tcb_schedule(tcb, 0, 0, tier)?;
        }
        let stub = procmgr::STUBS + 8 * slot as u32;
        let words = [0xe51f_f004u32, target];
        let at = app.stub_page as usize + 8 * slot;
        unsafe {
            (at as *mut u32).write_volatile(words[0]);
            ((at + 4) as *mut u32).write_volatile(words[1]);
        }
        app.space.sync_code(procmgr::STUBS, procmgr::STUBS + sys::PAGE as u32);
        regs.pc = stub;
        regs.cpsr = 0x10;
        app.threads.push(Thread {
            tid,
            tcb,
            reply: None,
            wait: None,
            deadline: None,
            saved: [0; 9],
            clear_tid: 0,
            alive: true,
            slot: slot as u32,
            sc,
        });
        sys::tcb_write(tcb, true, &regs)?;
        Ok(slot)
    }

    fn release_objects(&mut self, index: usize, slots: Vec<u32>) {
        // Revoking the app's kernel memory deletes every object made from it.
        sys::revoke(self.untypeds[index]);
        for slot in slots {
            self.k.release(slot);
        }
    }

    /// Reclaim an app slot and memory from the least recently used background app.
    /// Prefer apps without an open audio stream so music can continue playing.
    fn reclaim(&mut self, caller: Option<usize>) -> bool {
        let victim = self.apps.iter().enumerate()
            .filter(|(i, _)| Some(*i) != self.foreground && Some(*i) != self.home && Some(*i) != caller)
            .filter_map(|(i, app)| app.as_ref().map(|app| (i, app)))
            .filter(|(_, app)| app.path != appstore::INSTALLER && app.frozen.is_none())
            .min_by_key(|(_, app)| {
                let has_audio = app.fds.iter().flatten()
                    .any(|fd| matches!(fd.borrow().kind, device::Kind::Audio { .. }));
                (has_audio, app.used_ns)
            })
            .map(|(i, app)| (i, app.name.clone()));
        let Some((index, name)) = victim else { return false };
        self.reap(index);
        log!("low on app memory: closed {} in the background ({} KiB free)", name, self.pool.available() * 64);
        true
    }

    /// Tear an app down: its threads stop and its memory returns to the pool.
    pub fn reap(&mut self, index: usize) {
        let Some(mut app) = self.apps[index].take() else { return };
        for t in &app.threads {
            if t.alive {
                sys::tcb_suspend(t.tcb);
            }
            if let Some(r) = t.reply {
                sys::delete(r);
                self.k.release(r);
            }
        }
        if let Some(surface) = app.surface.take() {
            for p in surface.pages {
                self.pool.give_back(p);
            }
        }
        app.space.destroy(&mut self.k, &mut self.pool);
        self.release_objects(index, core::mem::take(&mut app.space.object_slots));
        log!("{} (pid {}) exited with status {}", app.name, app.pid, app.exit_code.unwrap_or(-1));
        if self.home == Some(index) {
            self.home = None;
        }
        if app.frozen.is_some() {
            self.sentinel.frozen_apps = self.sentinel.frozen_apps.saturating_sub(1);
        }
        let status = alloc::format!("status {}", app.exit_code.unwrap_or(-1));
        let ev = TelemetryEvent::new(sys::now_ns(), AppId(app.pid), EventKind::Exit, 0, status.as_bytes());
        self.shield.ring.emit(ev);
        if self.foreground == Some(index) {
            self.foreground = None;
            self.show_home();
        }
    }

    /// Answer a waiting thread and let it run again.
    pub fn wake(&mut self, index: usize, slot: usize, result: i32) {
        let Some(app) = self.apps[index].as_mut() else { return };
        let t = &mut app.threads[slot];
        let Some(reply) = t.reply.take() else { return };
        t.wait = None;
        t.deadline = None;
        let mut mrs = t.saved;
        mrs[0] = result as u32;
        sys::send(reply, &mrs);
        self.k.reply_done(reply);
    }

    fn handle_fault(&mut self, r: sys::Received) {
        let index = (r.badge >> 12) as usize;
        let slot = ((r.badge & 0xfff) - 1) as usize;
        if index >= MAX_APPS || self.apps[index].as_ref().map_or(true, |a| slot >= a.threads.len()) {
            return;
        }
        match r.label {
            sys::FAULT_UNKNOWN_SYSCALL => self.syscall(index, slot, &r.mrs),
            sys::FAULT_VM => {
                let (pc, addr) = (r.mrs[0], r.mrs[1]);
                let mut resolved = self.apps[index].as_mut().unwrap().space.fault(&mut self.k, &mut self.pool, addr);
                // Out of pages: close a background app and try again.
                let can_reclaim = self.foreground == Some(index)
                    && self.apps[index].as_ref().unwrap().space.find(addr).is_some_and(|v| v.prot != 0);
                while !resolved && can_reclaim && self.pool.available() == 0 && self.reclaim(None) {
                    resolved = self.apps[index].as_mut().unwrap().space.fault(&mut self.k, &mut self.pool, addr);
                }
                let app = self.apps[index].as_mut().unwrap();
                if resolved {
                    sys::reply(&[]);
                } else {
                    let (pages, tables) = app.space.usage();
                    log!("{} (pid {}): bad memory access at {:#x}, pc {:#x} ({} KiB free; it held {} KiB in {} tables)", app.name, app.pid, addr, pc, self.pool.available() * 64, pages * 64, tables);
                    app.exit_code = Some(139);
                    self.reap(index);
                }
            }
            _ => {
                let app = self.apps[index].as_mut().unwrap();
                log!("{} (pid {}): fault {} at pc {:#x}", app.name, app.pid, r.label, r.mrs[0]);
                app.exit_code = Some(132);
                self.reap(index);
            }
        }
    }

    /// Wake threads whose wait finished or timed out.
    fn check_waiters(&mut self) {
        let now = sys::now_ns();
        for index in 0..MAX_APPS {
            let Some(app) = self.apps[index].as_ref() else { continue };
            if app.frozen.is_some() {
                continue;
            }
            let waiting: Vec<(usize, Wait, Option<u64>)> = app
                .threads
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t.wait.clone().map(|w| (i, w, t.deadline)))
                .collect();
            for (slot, wait, deadline) in waiting {
                if let Some(result) = self.ready(index, &wait) {
                    self.wake(index, slot, result);
                } else if deadline.map_or(false, |d| now >= d) {
                    let result = match wait {
                        Wait::Futex { .. } => syscall::ETIMEDOUT,
                        Wait::Poll { .. } | Wait::Epoll { .. } | Wait::Sleep => 0,
                        Wait::Read { .. } | Wait::RecvFrom { .. } | Wait::Audio { .. } | Wait::Send { .. } => syscall::EAGAIN,
                        Wait::Connect { .. } => -110,
                        Wait::Frozen => syscall::EACCES,
                        Wait::Present { result } => result,
                        Wait::Vsync { .. } => 0,
                    };
                    self.wake(index, slot, result);
                }
            }
        }
    }

    /// On the handset, USB Diagnostics can ask for a program to be started.
    #[cfg(moto_board)]
    fn usb_requests(&mut self) {
        let page = 0x0900_1000 as *const u32;
        let serial = unsafe { page.read_volatile() };
        if serial == self.usb_serial {
            return;
        }
        self.usb_serial = serial;
        let len = (unsafe { page.add(1).read_volatile() } as usize).min(40);
        let mut bytes = [0u8; 40];
        for (i, b) in bytes[..len].iter_mut().enumerate() {
            *b = unsafe { (0x0900_1008 as *const u8).add(i).read_volatile() };
        }
        let path = String::from_utf8_lossy(&bytes[..len]).into_owned();
        if let Some(args) = path.strip_prefix("@shot") {
            self.usb_shot(args);
            return;
        }
        // The panel's scan line, sampled for 200 ms (frames seen, period).
        // "@sys frames" or "@sys bench tap X Y N": measurements, answered in the log.
        if let Some(line) = path.strip_prefix("@sys ") {
            // Shared settings too ("@sys set relay 192.168.1.20:8787" points the agent at a relay).
            if line.starts_with("frames") || line.starts_with("bench ") || line.starts_with("set ") || line.starts_with("get ") || line == "installed" || line == "memory" {
                let answer = self.system(self.foreground.unwrap_or(0), line);
                log!("{}: {}", line, answer.trim_end());
            }
            return;
        }
        // A new build replaces the copy that is running.
        for i in 0..MAX_APPS {
            if self.apps[i].as_ref().map_or(false, |a| a.path == path) {
                log!("replacing the running {}", path);
                self.reap(i);
            }
        }
        match self.launch(&path, &[], None) {
            Ok(i) => {
                if path == HOME_APP {
                    self.home = Some(i);
                }
                self.set_foreground(Some(i));
                self.shell.dirty = true;
            }
            Err(e) => log!("could not start {}: {}", path, e),
        }
    }
    #[cfg(not(moto_board))]
    fn usb_requests(&mut self) {}

    /// A screenshot over USB (handset), one strip per request: "@shot STRIP SCALE NONCE"
    /// copies screen rows, scaled down by SCALE, as RGB565 into the 64 KiB install
    /// window after a 32-byte header ("SHOT", nonce, strip, scale, width, height,
    /// rows per strip, rows here). The nonce is written last, so the host knows
    /// when the strip is complete.
    #[cfg(moto_board)]
    fn usb_shot(&self, args: &str) {
        let mut words = args.split_whitespace().map(|w| w.parse::<usize>().unwrap_or(0));
        let (strip, scale, nonce) = (words.next().unwrap_or(0), words.next().unwrap_or(2).clamp(1, 8), words.next().unwrap_or(0));
        let (w, h) = (device::WIDTH / scale, device::HEIGHT / scale);
        let per = (65536 - 32) / (w * 2);
        let (y0, y1) = ((strip * per).min(h), (strip * per + per).min(h));
        let window = 0x0950_1000 as *mut u8;
        let mut at = 32;
        for y in y0..y1 {
            let row = (sys::FRAMEBUFFER + y * scale * device::STRIDE) as *const u32;
            for x in 0..w {
                let p = unsafe { row.add(x * scale).read_volatile() };
                let v = (((p >> 19) & 0x1f) << 11 | ((p >> 10) & 0x3f) << 5 | ((p >> 3) & 0x1f)) as u16;
                unsafe {
                    window.add(at).write_volatile(v as u8);
                    window.add(at + 1).write_volatile((v >> 8) as u8);
                }
                at += 2;
            }
        }
        let head = window as *mut u32;
        let fields = [u32::from_le_bytes(*b"SHOT"), 0, strip as u32, scale as u32, w as u32, h as u32, per as u32, (y1 - y0) as u32];
        unsafe {
            for (i, f) in fields.iter().enumerate().skip(2) {
                head.add(i).write_volatile(*f);
            }
            head.write_volatile(fields[0]);
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            head.add(1).write_volatile(nonce as u32);
        }
    }

    /// Installs over USB (handset): append the 64 KiB window to /apps/NAME.
    /// Control words: 0 commit serial (USB), 1 done serial, 2 result; file
    /// offset at byte 16, length at 20, the name from byte 24.
    #[cfg(moto_board)]
    fn usb_installs(&mut self) {
        let control = 0x0950_0000 as *mut u32;
        let (commit, done) = unsafe { (control.read_volatile(), control.add(1).read_volatile()) };
        if commit == done {
            return;
        }
        let _lowered = sys::Lowered::new();
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let (offset, len) = unsafe { (control.add(4).read_volatile() as usize, control.add(5).read_volatile() as usize) };
        let mut name = [0u8; 32];
        for (i, b) in name.iter_mut().enumerate() {
            *b = unsafe { (0x0950_0018 as *const u8).add(i).read_volatile() };
        }
        let end = name.iter().position(|&b| b == 0).unwrap_or(32);
        let name = String::from_utf8_lossy(&name[..end]).into_owned();
        let path = alloc::format!("/apps/{}", name);
        // Blocks go to a hidden staging file; an empty block at the end swaps
        // it into place, so a push that fails leaves the working copy alone.
        let staged = alloc::format!("/apps/.{}.part", name);
        let data = unsafe { core::slice::from_raw_parts(0x0950_1000 as *const u8, len.min(65536)) };
        let valid = !name.is_empty() && !name.contains('/') && !name.starts_with('.');
        if valid && len == 0 && offset > 0 {
            let ok = self.fs.contents(&staged).map_or(false, |d| d.len() == offset) && self.fs.rename(&staged, &path).is_ok();
            if ok {
                log!("installed {} ({} bytes)", path, offset);
                self.shell.dirty = true;
            }
            unsafe {
                control.add(2).write_volatile(if ok { 0 } else { 1 });
                core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
                control.add(1).write_volatile(commit);
            }
            return;
        }
        let ok = valid && match self.fs.writable(&staged, true, offset == 0, 0) {
            Some(file) => {
                if file.len() < offset {
                    false
                } else {
                    file.truncate(offset);
                    // Grow in 1 MiB steps rather than doubling, and fail the
                    // install (not the runtime) when the heap is full.
                    let want = file.len() + data.len();
                    let step = if want > file.capacity() { (want - file.len()).max(1 << 20) } else { 0 };
                    if file.try_reserve_exact(step).is_err() {
                        log!("not enough memory to install {}", path);
                        false
                    } else {
                        file.extend_from_slice(data);
                        true
                    }
                }
            }
            None => false,
        };
        if ok && offset == 0 {
            log!("installing {}", path);
        }
        unsafe {
            control.add(2).write_volatile(if ok { 0 } else { 1 });
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            control.add(1).write_volatile(commit);
        }
    }
    #[cfg(not(moto_board))]
    fn usb_installs(&mut self) {}

    fn poll_runtime(&mut self) {
        let t0 = sys::now_ns();
        data::flush(&mut self.fs);
        if !self.radios_loaded {
            // Before the auto-connector or Phone Internet first look.
            self.radios_loaded = true;
            let saved = self.fs.contents(radios::SAVED).map(|d| String::from_utf8_lossy(d).into_owned()).unwrap_or_default();
            self.apply_radios(radios::Radios::parse(&saved));
        }
        self.wifi.tick(&self.fs, t0);
        self.usb_requests();
        self.usb_installs();
        let t1 = sys::now_ns();
        let focus = request(GET, FOCUS, 0);
        let t2 = sys::now_ns();
        self.stats.step(0, t1 - t0);
        self.stats.step(1, t2 - t1);
        let focused = focus.op == OK && focus.value == OPEN_APPS;
        if focused != self.focused {
            log!("in front: {} (focus {} op {})", focused, focus.value, focus.op);
        }
        if focused && !self.focused && !self.home_tried {
            // Home starts the first time the open apps come to the front, so it
            // costs nothing on a system that never shows them.
            self.home_tried = true;
            self.start_home();
        }
        if focused && !self.focused {
            self.shell.dirty = true;
            self.shell.armed = false;
            if self.ring.live() {
                // Interrupt-driven touch sends no idle samples to arm the
                // shell; a finger already lifted counts as lifted.
                self.shell.arm_with(self.ring.down);
            }
            if let Some(i) = self.foreground {
                self.refresh(i);
                self.lifecycle(i, abi::LIFECYCLE_FOCUS_GAINED);
            }
        }
        if !focused && self.focused {
            if let Some(i) = self.foreground {
                self.lifecycle(i, abi::LIFECYCLE_FOCUS_LOST);
            }
        }
        self.focused = focused;
        if !focused {
            self.frames.last_poll_ns = 0;
            return;
        }
        self.frames.polled(t0);
        if self.contact.is_none() {
            let r = Message::new(SUBSCRIBE, CONTACT, 0).call(sys::SLOT_RUNTIME);
            if r.op == OK {
                self.contact = Some(r.value);
            }
        }
        // Touch comes from Touch's interrupt ring; poll Runtime only if the
        // ring is not running, or to confirm a release that never arrived.
        // Still ask every 500 ms: Touch kicks the hardware watchdog when it
        // serves a request, so this chain (Runtime, its worker, Touch) is
        // also how the phone knows the system is alive.
        let stuck = self.ring.down && t0.wrapping_sub(self.ring.last_ns) > 200_000_000;
        let liveness = t0.wrapping_sub(self.ring.polled_ns) > 500_000_000;
        let ring_ok = self.ring.live() && !stuck && !liveness;
        if !ring_ok && self.ring.live() {
            self.ring.polled_ns = t0;
        }
        if let Some(stream) = self.contact.filter(|_| !ring_ok) {
            let t = sys::now_ns();
            // Runtime answers a touch poll with the sample its worker read
            // last and starts the next read; give that worker one turn so
            // this poll delivers the current sample, not one a poll old.
            let mut waited = false;
            for _ in 0..8 {
                let e = Message::new(POLL, 0, stream).call(sys::SLOT_RUNTIME);
                if e.op != OK {
                    if waited {
                        break;
                    }
                    waited = true;
                    sys::pause();
                    continue;
                }
                self.touch(e.value as i32, e.arg as i32, e.key != 0);
            }
            self.stats.step(2, sys::now_ns() - t);
        }
        if self.foreground.is_none() && self.shell.dirty {
            self.draw_shell();
        }
    }

    /// MCS priority tiers (docs/8ms-frame-pipeline-rfc.md): the app on screen
    /// runs at 120 with 6 ms of every 8, ahead of the services at 100 but
    /// never starving them; every other app runs round robin at 100.
    fn set_tiers(&mut self) {
        let front = self.foreground.filter(|_| self.focused);
        if front == self.tiered || (!sys::mcs() && sys::cores() == 1) {
            return;
        }
        for (i, front) in [(self.tiered, false), (front, true)] {
            let Some(app) = i.and_then(|i| self.apps[i].as_ref()) else { continue };
            let tier = if front { sys::tier_foreground() } else { sys::tier_background(i.unwrap()) };
            for t in app.threads.iter().filter(|t| t.alive && (t.sc != 0 || !sys::mcs())) {
                let _ = sys::tcb_schedule(t.tcb, t.sc, 0, tier);
            }
        }
        self.tiered = front;
    }

    /// Runtime polls (focus, and touch without the interrupt ring): every
    /// 4 ms while apps run, 16 ms when touch arrives by interrupt, 50 ms idle.
    fn poll_interval(&self, busy: bool) -> u64 {
        match (busy, self.ring.live()) {
            (false, _) => 50_000_000,
            (true, true) => 16_000_000,
            (true, false) => 4_000_000,
        }
    }

    /// Reports Touch has put in the ring since the last pass, to the app in front.
    fn drain_touch(&mut self) {
        while let Some((x, y, down, delay_ns)) = self.ring.next() {
            if self.focused {
                self.frames.touch_irq.add(delay_ns);
                self.touch(x, y, down);
            }
        }
    }

    /// Synthetic presses from `bench tap`: a touch down on the app in front,
    /// then, 150 ms later, focus handed back so it drops the press.
    fn bench_tick(&mut self, now: u64) {
        let Some(mut b) = self.frames.bench else { return };
        if now < b.next_ns {
            return;
        }
        let Some(i) = self.foreground.filter(|_| self.focused) else {
            // `bench press` on the app list taps it like a finger would.
            if b.release && self.focused && self.foreground.is_none() {
                self.touch(b.x, b.y, !b.pressed);
                b.pressed = !b.pressed;
                if !b.pressed {
                    b.left -= 1;
                }
                b.next_ns = now + if b.pressed { 120_000_000 } else { 600_000_000 };
                self.frames.bench = (b.left > 0).then_some(b);
                return;
            }
            self.frames.bench = None;
            return;
        };
        let touch = |kind, y| abi::InputEvent { timestamp_us: now / 1000, kind, code: 0, x: b.x, y, value: 0 };
        if b.pressed && b.dy != 0 {
            // A drag: a move every 8 ms, 250 ms in all, then the finger lifts.
            b.step += 1;
            let y = b.y + b.dy * b.step.min(31) / 31;
            if b.step <= 31 {
                self.event(i, touch(abi::EVENT_TOUCH_MOVE, y));
                b.next_ns = now + 8_000_000;
            } else {
                self.event(i, touch(abi::EVENT_TOUCH_UP, y));
                b.pressed = false;
                b.step = 0;
                b.left -= 1;
                b.next_ns = now + 1_500_000_000;
            }
        } else if b.pressed && b.release {
            self.event(i, touch(abi::EVENT_TOUCH_UP, b.y));
            b.pressed = false;
            b.left -= 1;
            b.next_ns = now + 600_000_000;
        } else if b.pressed {
            self.lifecycle(i, abi::LIFECYCLE_FOCUS_GAINED);
            b.pressed = false;
            b.left -= 1;
            b.next_ns = now + 350_000_000;
        } else {
            self.event(i, touch(abi::EVENT_TOUCH_DOWN, b.y));
            b.pressed = true;
            b.next_ns = now + if b.dy != 0 { 8_000_000 } else { 150_000_000 };
        }
        self.frames.bench = (b.left > 0).then_some(b);
    }

    fn run(&mut self) -> ! {
        loop {
            let mut handled = 0;
            while handled < 64 {
                let r = sys::receive(false);
                if r.badge == 0 {
                    break;
                }
                if r.badge & sys::NOTIFY_BADGES != 0 {
                    if r.badge & sys::TIMER_BADGE != 0 {
                        sys::timer_disarm(); // an expiry that came while we worked
                    }
                    continue;
                }
                self.handle_fault(r);
                handled += 1;
            }
            let now = sys::now_ns();
            self.stats.loops += 1;
            self.stats.faults += handled as u64;
            if self.stats.since == 0 {
                self.stats.since = now;
            }
            if now.wrapping_sub(self.stats.since) >= 30_000_000_000 && self.apps.iter().any(|a| a.is_some()) {
                let secs = (now - self.stats.since) / 1_000_000_000;
                let (blocks, free) = heap::HEAP.stats();
                let waiting: usize = self.apps.iter().flatten().map(|a| a.threads.iter().filter(|t| t.wait.is_some()).count()).sum();
                let threads: usize = self.apps.iter().flatten().map(|a| a.threads.iter().filter(|t| t.alive).count()).sum();
                let mut calls = core::mem::take(&mut self.stats.calls);
                calls.sort_by_key(|c| core::cmp::Reverse(c.1));
                let top: Vec<String> = calls.iter().take(4).map(|(n, c)| alloc::format!("{}:{}/s", n, *c as u64 / secs)).collect();
                log!(
                    "stats: {} loops/s, asleep {}%, {} faults/s [{}], slowest poll {} us (flush {} focus {} touch {}), slowest pass {} us, {} threads ({} waiting), heap {} free blocks {} KiB",
                    self.stats.loops / secs, self.stats.slept_ns / 10_000_000 / secs, self.stats.faults / secs, top.join(" "), self.stats.slowest_poll / 1000,
                    self.stats.slowest_step[0] / 1000, self.stats.slowest_step[1] / 1000, self.stats.slowest_step[2] / 1000,
                    self.stats.slowest_pass / 1000, threads, waiting, blocks, free / 1024
                );
                self.stats = Stats { since: now, ..Stats::default() };
                if self.frames.present.count > 0 && self.frames.bench.is_none() {
                    let report = self.frames.report(now);
                    log!("frames: {}", report.trim_end());
                }
            }
            if self.stats.pass_start != 0 {
                self.stats.slowest_pass = self.stats.slowest_pass.max(now.wrapping_sub(self.stats.pass_start));
            }
            self.stats.pass_start = now;
            // Poll Runtime often while an app runs or the app list is on screen;
            // otherwise rarely, so an idle Open Runtime costs the other processes little.
            let busy = self.focused || self.apps.iter().any(|a| a.is_some());
            let interval = self.poll_interval(busy);
            if now.wrapping_sub(self.last_poll) >= interval {
                self.last_poll = now;
                self.poll_runtime();
                self.stats.slowest_poll = self.stats.slowest_poll.max(sys::now_ns().wrapping_sub(now));
            }
            self.drain_touch();
            self.set_tiers();
            self.bench_tick(now);
            self.beam.tick();
            self.flush_presents();
            self.check_waiters();
            if now.wrapping_sub(self.last_sentinel) >= 100_000_000 {
                self.last_sentinel = now;
                self.shield_tick(now);
            }
            if handled == 0 {
                self.idle(now);
            }
        }
    }

    /// Nothing left to do this pass. With the timer, sleep until the next
    /// Runtime poll, thread deadline or frame, or until an app calls; at
    /// EVENT_PRIORITY the wake-up runs ahead of the polling services. Without
    /// it, yield as before.
    fn idle(&mut self, now: u64) {
        if !sys::timer_ready() {
            return sys::yield_now();
        }
        let sleep = self.next_wake(now).saturating_sub(sys::now_ns());
        if sleep < 50_000 {
            return;
        }
        sys::timer_arm(sleep);
        let r = sys::receive(true);
        sys::timer_disarm();
        let woke = sys::now_ns();
        self.stats.slept_ns += woke - now;
        self.stats.pass_start = 0; // a pass measures work, not sleep
        if r.badge != 0 && r.badge & sys::NOTIFY_BADGES == 0 {
            self.handle_fault(r);
        }
    }

    /// When the loop must run next: the Runtime poll (touch, focus), the
    /// Shield tick, thread deadlines, presents waiting on the panel, frames
    /// for VSync waiters, and a short interval while a thread waits on
    /// something only polling notices (sockets, the audio ring, poll/epoll).
    fn next_wake(&mut self, now: u64) -> u64 {
        let busy = self.focused || self.apps.iter().any(|a| a.is_some());
        let mut wake = self.last_poll + self.poll_interval(busy);
        wake = wake.min(self.last_sentinel + 100_000_000);
        if let Some(b) = self.frames.bench {
            wake = wake.min(b.next_ns);
        }
        let next_frame = self.beam.frame_ns + u64::from(self.beam.period_us()) * 1000 + 200_000;
        for app in self.apps.iter().flatten() {
            if let Some(([_, y0, _, y1], _)) = app.surface.as_ref().and_then(|s| s.pending) {
                // Just as the beam leaves the rows.
                wake = wake.min(now + self.beam.wait_ns(y0, y1).max(50_000));
            }
            for t in app.threads.iter().filter(|t| t.alive) {
                let Some(w) = &t.wait else { continue };
                if let Some(d) = t.deadline {
                    wake = wake.min(d);
                }
                match w {
                    Wait::Futex { .. } | Wait::Sleep | Wait::Frozen | Wait::Present { .. } => {}
                    Wait::Vsync { .. } => wake = wake.min(next_frame.max(now + 500_000)),
                    _ => wake = wake.min(now + 2_000_000),
                }
            }
        }
        wake.min(now + 50_000_000)
    }
}

#[derive(Default)]
pub struct Stats {
    since: u64,
    /// The busiest system calls: (number, count).
    calls: Vec<(u32, u32)>,
    /// The slowest step of a Runtime poll: flush, focus, touch.
    slowest_step: [u64; 3],
    loops: u64,
    faults: u64,
    slowest_poll: u64,
    slowest_pass: u64,
    pass_start: u64,
    /// Time spent asleep waiting for events (with the timer).
    slept_ns: u64,
}

impl Stats {
    pub fn count(&mut self, number: u32) {
        if let Some(c) = self.calls.iter_mut().find(|c| c.0 == number) {
            c.1 += 1;
        } else if self.calls.len() < 64 {
            self.calls.push((number, 1));
        }
    }
    fn step(&mut self, i: usize, ns: u64) {
        self.slowest_step[i] = self.slowest_step[i].max(ns);
    }
}

/// Enough heap to report a failed start before the pool exists (the pool
/// then adds HEAP_PAGES of its own; see pool.rs).
/// 7.9 KiB: with the other statics, the package (code and zeroed data) must
/// stay within its 0x3e000-byte window.
static mut BOOT_HEAP: [u64; 1008] = [0; 1008];

pub fn run() -> ! {
    Log::init();
    unsafe { heap::HEAP.add(core::ptr::addr_of_mut!(BOOT_HEAP) as usize, 1008 * 8) };
    let mut rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            Log::write(alloc::format!("[open] start failed: seL4 error {}\n", e).as_bytes());
            loop {
                sys::yield_now();
            }
        }
    };
    let inventory = request(GET, INVENTORY, 0);
    rt.audio = inventory.op == OK && inventory.value & (1 << AUDIO_COMPONENT) != 0;
    rt.camera = inventory.op == OK && inventory.value & (1 << CAMERA_COMPONENT) != 0;
    log!(
        "ready: {} files in /apps image, {} MiB for apps",
        rt.fs.files(),
        rt.pool.available() / 16
    );
    let _ = rt.policy.check(policy::AppId(0), &policy::Action::SpawnApp { path: "/apps" });
    if sys::timer_selftest() {
        sys::set_own_priority(sys::EVENT_PRIORITY);
        log!("event-driven: the timer works; waiting for events at priority {}", sys::EVENT_PRIORITY);
    } else {
        log!("no timer: polling at priority {}", sys::PRIORITY);
    }
    log!(
        "scheduling: {}, {} {}",
        if sys::mcs() { "MCS budgets" } else { "static priorities" },
        sys::cores(),
        if sys::cores() > 1 { "cores (the app in front on core 1, others on 2-3)" } else { "core" }
    );
    rt.run()
}
