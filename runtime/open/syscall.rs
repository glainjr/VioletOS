//! Linux ARM EABI system calls for open apps.
//!
//! seL4's own system calls use negative numbers in r7; Linux uses positive
//! ones (plus 0xf0000 and up for ARM private calls). A Linux `svc #0` reaches
//! this process as an unknown-syscall fault carrying r0-r7. The answer goes
//! back in r0. seL4 restarts a faulted thread at FaultIP, which it records as
//! the svc address minus 4 in either instruction set, so the reply sets
//! FaultIP four bytes on: the instruction after the svc.
use super::abi::*;
use super::device::{Kind, OpenFile, BUFFER_BYTES, HEIGHT, STRIDE, WIDTH};
use super::fs::{self, S_IFCHR, S_IFDIR, S_IFMT};
use super::policy::{AccessMode, Action, AppId, Decision, Policy};
use super::procmgr::{PROT_READ, PROT_WRITE, USER_TOP};
use super::sys::{self, MR_CPSR, MR_FAULT_IP};
use super::{FileRef, Log, Runtime, Wait};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

pub const EPERM: i32 = -1;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const EIO: i32 = -5;
pub const EBADF: i32 = -9;
pub const ECHILD: i32 = -10;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const EEXIST: i32 = -17;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const EMFILE: i32 = -24;
pub const ENOTTY: i32 = -25;
pub const ESPIPE: i32 = -29;
pub const ERANGE: i32 = -34;
pub const ENOSYS: i32 = -38;
pub const ENOTEMPTY: i32 = -39;
pub const ETIMEDOUT: i32 = -110;
pub const ENETDOWN: i32 = -100;
pub const EBUSY: i32 = -16;
pub const EAFNOSUPPORT: i32 = -97;

const AT_FDCWD: i32 = -100;
const O_ACCMODE: u32 = 3;
const O_CREAT: u32 = 0o100;
const O_EXCL: u32 = 0o200;
const O_TRUNC: u32 = 0o1000;
const O_APPEND: u32 = 0o2000;
const O_NONBLOCK: u32 = 0o4000;
const O_DIRECTORY: u32 = 0o40000; // ARM's value.
const CLONE_VM: u32 = 0x100;
const CLONE_SETTLS: u32 = 0x80000;
const CLONE_PARENT_SETTID: u32 = 0x100000;
const CLONE_CHILD_CLEARTID: u32 = 0x200000;
const CLONE_CHILD_SETTID: u32 = 0x1000000;
const MAP_SHARED: u32 = 1;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;
const MAP_FIXED_NOREPLACE: u32 = 0x100000;

/// Seconds since 1970 when this image was built, as the wall clock's start.
fn epoch() -> u64 {
    option_env!("VIOLET_BUILD_EPOCH").and_then(|s| s.parse().ok()).unwrap_or(1_795_000_000)
}

pub enum Outcome {
    Done(i32),
    Block(Wait, Option<u64>),
    /// The thread or its whole app ended; nothing to answer.
    Gone,
}
use Outcome::*;

fn align_up(v: u32, a: u32) -> u32 {
    v.wrapping_add(a - 1) & !(a - 1)
}

impl Runtime {
    pub fn syscall(&mut self, i: usize, slot: usize, mrs: &[u32; 16]) {
        let mut saved = [0u32; 9];
        saved[..8].copy_from_slice(&mrs[..8]);
        saved[8] = mrs[MR_FAULT_IP].wrapping_add(4);
        let number = mrs[7];
        self.stats.count(number);
        // A frozen app's thread that reached here before it was suspended waits too.
        let outcome = if self.apps[i].as_ref().map_or(false, |a| a.frozen.is_some()) {
            Block(Wait::Frozen, None)
        } else {
            self.dispatch(i, slot, number, mrs)
        };
        match outcome {
            Done(r) => {
                saved[0] = r as u32;
                sys::reply(&saved);
            }
            Block(wait, deadline) => {
                let reply = self.k.slot();
                if sys::save_caller(reply).is_err() || (sys::mcs() && self.k.refill_reply().is_err()) {
                    self.k.release(reply);
                    saved[0] = EAGAIN as u32;
                    sys::reply(&saved);
                    return;
                }
                let t = &mut self.apps[i].as_mut().unwrap().threads[slot];
                t.saved = saved;
                t.reply = Some(reply);
                t.wait = Some(wait);
                t.deadline = deadline;
            }
            Gone => {}
        }
    }

    // ---- app memory helpers ----
    fn rd(&mut self, i: usize, va: u32, len: usize) -> Option<Vec<u8>> {
        let mut v = alloc::vec![0u8; len];
        let app = self.apps[i].as_mut()?;
        app.space.read(&mut self.k, &mut self.pool, va, &mut v).then_some(v)
    }
    fn wr(&mut self, i: usize, va: u32, data: &[u8]) -> bool {
        let app = self.apps[i].as_mut().unwrap();
        app.space.write(&mut self.k, &mut self.pool, va, data)
    }
    fn rd32(&mut self, i: usize, va: u32) -> Option<u32> {
        let app = self.apps[i].as_mut()?;
        app.space.read_u32(&mut self.k, &mut self.pool, va)
    }
    fn wr32(&mut self, i: usize, va: u32, v: u32) -> bool {
        self.wr(i, va, &v.to_le_bytes())
    }
    fn rstr(&mut self, i: usize, va: u32) -> Option<String> {
        let app = self.apps[i].as_mut()?;
        app.space.read_string(&mut self.k, &mut self.pool, va, 4096)
    }
    fn file(&self, i: usize, fd: i32) -> Option<FileRef> {
        self.apps[i].as_ref()?.fds.get(fd as usize)?.clone()
    }
    fn install(&mut self, i: usize, f: OpenFile, from: usize) -> i32 {
        let app = self.apps[i].as_mut().unwrap();
        let file = Rc::new(RefCell::new(f));
        for (n, slot) in app.fds.iter_mut().enumerate().skip(from) {
            if slot.is_none() {
                *slot = Some(file);
                return n as i32;
            }
        }
        if app.fds.len() >= 1024 {
            return EMFILE;
        }
        while app.fds.len() < from {
            app.fds.push(None);
        }
        app.fds.push(Some(file));
        app.fds.len() as i32 - 1
    }
    /// An absolute path for `path` relative to `dirfd` (or the working directory).
    fn at(&mut self, i: usize, dirfd: i32, path_ptr: u32) -> Result<String, i32> {
        let path = self.rstr(i, path_ptr).ok_or(EFAULT)?;
        let base = if path.starts_with('/') || dirfd == AT_FDCWD {
            self.apps[i].as_ref().unwrap().cwd.clone()
        } else {
            let f = self.file(i, dirfd).ok_or(EBADF)?;
            let f = f.borrow();
            match &f.kind {
                Kind::Dir { path, .. } | Kind::File { path } => path.clone(),
                _ => return Err(ENOTDIR),
            }
        };
        Ok(fs::normalize(&base, &path))
    }
    fn timespec(&mut self, i: usize, ptr: u32, time64: bool) -> Option<u64> {
        if time64 {
            let b = self.rd(i, ptr, 12)?;
            let sec = i64::from_le_bytes(b[..8].try_into().unwrap());
            let nsec = u32::from_le_bytes(b[8..12].try_into().unwrap());
            Some((sec.max(0) as u64) * 1_000_000_000 + u64::from(nsec))
        } else {
            let b = self.rd(i, ptr, 8)?;
            let sec = i32::from_le_bytes(b[..4].try_into().unwrap());
            let nsec = u32::from_le_bytes(b[4..8].try_into().unwrap());
            Some((sec.max(0) as u64) * 1_000_000_000 + u64::from(nsec))
        }
    }
    fn put_timespec(&mut self, i: usize, ptr: u32, ns: u64, time64: bool) -> bool {
        let (sec, nsec) = (ns / 1_000_000_000, (ns % 1_000_000_000) as u32);
        if time64 {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&(sec as i64).to_le_bytes());
            b[8..12].copy_from_slice(&nsec.to_le_bytes());
            self.wr(i, ptr, &b)
        } else {
            let mut b = [0u8; 8];
            b[..4].copy_from_slice(&(sec as i32).to_le_bytes());
            b[4..].copy_from_slice(&nsec.to_le_bytes());
            self.wr(i, ptr, &b)
        }
    }
    pub fn clock(&self, id: u32) -> u64 {
        let mono = sys::now_ns();
        match id {
            0 | 5 | 8 | 11 => epoch() * 1_000_000_000 + mono,
            _ => mono,
        }
    }

    fn dispatch(&mut self, i: usize, slot: usize, n: u32, m: &[u32; 16]) -> Outcome {
        let a = [m[0], m[1], m[2], m[3], m[4], m[5], m[6]];
        let tid = self.apps[i].as_ref().unwrap().threads[slot].tid;
        match n {
            // ---- process and threads ----
            1 => self.exit_thread(i, slot),
            248 => {
                let app = self.apps[i].as_mut().unwrap();
                app.exit_code = Some(a[0] as i32 & 0xff);
                self.reap(i);
                Gone
            }
            2 | 11 | 190 | 387 => Done(ENOSYS), // fork, execve, vfork, execveat
            120 => self.clone(i, slot, a, m),
            435 => Done(ENOSYS), // clone3: callers fall back to clone
            224 => Done(tid as i32),
            20 => Done(self.apps[i].as_ref().unwrap().pid as i32),
            64 => Done(1),
            65 | 132 | 147 | 66 => Done(self.apps[i].as_ref().unwrap().pid as i32),
            57 => Done(0),
            24 | 47 | 49 | 50 | 199 | 200 | 201 | 202 => Done(0),
            205 | 80 => Done(0),
            256 => {
                self.apps[i].as_mut().unwrap().threads[slot].clear_tid = a[0];
                Done(tid as i32)
            }
            338 | 172 | 97 | 96 | 241 | 154 | 156 => Done(0),
            155 | 157 => Done(0),
            159 | 160 => Done(0),
            398 | 389 | 424 | 434 => Done(ENOSYS),
            242 => {
                // One CPU.
                let len = a[1].min(128);
                let mut mask = alloc::vec![0u8; len as usize];
                if len == 0 {
                    return Done(EINVAL);
                }
                mask[0] = 1;
                if !self.wr(i, a[2], &mask) {
                    return Done(EFAULT);
                }
                Done(len.min(4) as i32)
            }
            345 => {
                if a[0] != 0 { self.wr32(i, a[0], 0); }
                if a[1] != 0 { self.wr32(i, a[1], 0); }
                Done(0)
            }
            37 | 238 | 268 => {
                let sig = if n == 268 { a[2] } else { a[1] };
                match sig {
                    0 | 23 | 28 | 17 => Done(0), // probe, SIGURG, SIGWINCH, SIGCHLD
                    _ => {
                        let app = self.apps[i].as_mut().unwrap();
                        crate::log!("{} (pid {}) killed by signal {}", app.name, app.pid, sig);
                        app.exit_code = Some(128 + sig as i32);
                        self.reap(i);
                        Gone
                    }
                }
            }
            114 => Done(ECHILD),
            // ---- signals: accepted, never delivered ----
            174 => {
                if a[2] != 0 {
                    self.wr(i, a[2], &[0u8; 20]);
                }
                Done(0)
            }
            175 | 126 => {
                if a[2] != 0 {
                    self.wr(i, a[2], &[0u8; 8]);
                }
                Done(0)
            }
            186 => {
                if a[1] != 0 {
                    // ss_sp, ss_flags = SS_DISABLE, ss_size.
                    self.wr(i, a[1], &[0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]);
                }
                Done(0)
            }
            176 => {
                self.wr(i, a[0], &[0u8; 8]);
                Done(0)
            }
            173 | 119 => Done(ENOSYS),
            // ---- memory ----
            45 => {
                let app = self.apps[i].as_mut().unwrap();
                Done(app.space.set_brk(&mut self.k, &mut self.pool, a[0]) as i32)
            }
            192 => self.mmap(i, a[0], a[1], a[2], a[3], a[4] as i32, a[5].wrapping_mul(4096)),
            91 => {
                if a[0] & 0xfff != 0 || a[1] == 0 {
                    return Done(EINVAL);
                }
                let end = a[0].saturating_add(align_up(a[1], 4096)).min(USER_TOP);
                let app = self.apps[i].as_mut().unwrap();
                app.space.remove(&mut self.k, &mut self.pool, a[0], end);
                Done(0)
            }
            125 => {
                if a[2] & 6 == 6 {
                    match self.gate(i, &Action::MapWriteExec { addr: a[0], len: a[1] }) {
                        super::Gate::Allow => {}
                        super::Gate::Deny => return Done(EACCES),
                        super::Gate::Frozen => return Block(Wait::Frozen, None),
                        super::Gate::Killed => return Gone,
                    }
                }
                let end = a[0].saturating_add(align_up(a[1], 4096));
                let app = self.apps[i].as_mut().unwrap();
                if app.space.protect(a[0], end, a[2] & 7) {
                    if a[2] & 4 != 0 {
                        app.space.sync_code(a[0], end);
                    }
                    Done(0)
                } else {
                    Done(ENOMEM)
                }
            }
            163 => Done(ENOMEM), // mremap: callers copy instead
            220 | 144 | 150 | 151 | 152 | 153 | 390 => Done(0),
            219 => Done(ENOSYS),
            0xf0002 => {
                let app = self.apps[i].as_mut().unwrap();
                app.space.sync_code(a[0], a[1]);
                Done(0)
            }
            0xf0005 => {
                let tcb = self.apps[i].as_ref().unwrap().threads[slot].tcb;
                Done(if sys::tcb_set_tls(tcb, a[0]).is_ok() { 0 } else { EINVAL })
            }
            // ---- time ----
            263 | 403 => {
                let time64 = n == 403;
                let now = self.clock(a[0]);
                Done(if self.put_timespec(i, a[1], now, time64) { 0 } else { EFAULT })
            }
            264 | 406 => {
                if a[1] != 0 {
                    self.put_timespec(i, a[1], 1, n == 406);
                }
                Done(0)
            }
            78 => {
                let now = self.clock(0);
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&((now / 1_000_000_000) as u32).to_le_bytes());
                b[4..].copy_from_slice(&(((now % 1_000_000_000) / 1000) as u32).to_le_bytes());
                if a[0] != 0 {
                    self.wr(i, a[0], &b);
                }
                Done(0)
            }
            13 => Done((self.clock(0) / 1_000_000_000) as i32),
            162 => {
                let Some(d) = self.timespec(i, a[0], false) else { return Done(EFAULT) };
                Block(Wait::Sleep, Some(sys::now_ns() + d))
            }
            265 | 407 => {
                let time64 = n == 407;
                let Some(t) = self.timespec(i, a[2], time64) else { return Done(EFAULT) };
                let deadline = if a[1] & 1 != 0 {
                    // Absolute: convert the wall clock to the monotonic one.
                    let now = self.clock(a[0]);
                    sys::now_ns() + t.saturating_sub(now)
                } else {
                    sys::now_ns() + t
                };
                Block(Wait::Sleep, Some(deadline))
            }
            158 => Done(0),
            77 => {
                self.wr(i, a[1], &[0u8; 72]);
                Done(0)
            }
            116 => {
                let mut b = [0u8; 64];
                let up = (sys::now_ns() / 1_000_000_000) as u32;
                b[..4].copy_from_slice(&up.to_le_bytes());
                let total = (self.pool.total * sys::PAGE) as u32;
                let free = (self.pool.available() * sys::PAGE) as u32;
                b[16..20].copy_from_slice(&total.to_le_bytes());
                b[20..24].copy_from_slice(&free.to_le_bytes());
                b[40..42].copy_from_slice(&1u16.to_le_bytes());
                b[52..56].copy_from_slice(&1u32.to_le_bytes());
                self.wr(i, a[0], &b);
                Done(0)
            }
            // ---- futexes ----
            240 | 422 => self.futex(i, a, n == 422),
            // ---- files ----
            5 => self.open(i, AT_FDCWD, a[0], a[1]),
            322 | 437 => {
                let flags = if n == 437 { self.rd32(i, a[2]).unwrap_or(0) } else { a[2] };
                self.open(i, a[0] as i32, a[1], flags)
            }
            8 => self.open(i, AT_FDCWD, a[0], O_CREAT | O_TRUNC | 1),
            6 => {
                let app = self.apps[i].as_mut().unwrap();
                match app.fds.get_mut(a[0] as usize) {
                    Some(f @ Some(_)) => {
                        *f = None;
                        Done(0)
                    }
                    _ => Done(EBADF),
                }
            }
            436 => {
                let app = self.apps[i].as_mut().unwrap();
                for fd in a[0]..=a[1].min(app.fds.len() as u32) {
                    if let Some(f) = app.fds.get_mut(fd as usize) {
                        *f = None;
                    }
                }
                Done(0)
            }
            3 => self.read(i, a[0] as i32, a[1], a[2]),
            4 => {
                let r = self.write(i, a[0] as i32, a[1], a[2], None);
                let blocking_audio = r == EAGAIN && self.file(i, a[0] as i32).map_or(false, |f| {
                    let f = f.borrow();
                    matches!(f.kind, Kind::Audio { .. }) && f.flags & O_NONBLOCK == 0
                });
                if blocking_audio {
                    return Block(Wait::Audio { fd: a[0] as i32, buf: a[1], len: a[2] }, None);
                }
                let blocking_socket = r == EAGAIN && self.file(i, a[0] as i32).map_or(false, |f| {
                    let f = f.borrow();
                    matches!(f.kind, Kind::Tcp { .. } | Kind::Hci) && f.flags & O_NONBLOCK == 0
                });
                if blocking_socket {
                    return Block(Wait::Send { fd: a[0] as i32, buf: a[1], len: a[2] }, None);
                }
                // A present the panel is still reading answers once it is copied.
                let screen = self.file(i, a[0] as i32).map_or(false, |f| matches!(f.borrow().kind, Kind::Screen));
                if screen && r > 0 && self.present_pending(i) {
                    return Block(Wait::Present { result: r }, None);
                }
                Done(r)
            }
            145 | 146 => {
                let mut total = 0i32;
                for k in 0..a[2] {
                    let (Some(base), Some(len)) = (self.rd32(i, a[1] + 8 * k), self.rd32(i, a[1] + 8 * k + 4)) else {
                        return Done(EFAULT);
                    };
                    if len == 0 {
                        continue;
                    }
                    let r = if n == 146 {
                        self.write(i, a[0] as i32, base, len, None)
                    } else {
                        self.read_now(i, a[0] as i32, base, len)
                    };
                    if r == EAGAIN && total == 0 {
                        // Nothing moved on a blocking socket or pipe: wait, as write and
                        // read do, with this buffer (a short count is fine for writev).
                        // rustls writes TLS records with writev, and a full send ring
                        // used to come back as EAGAIN on a blocking socket.
                        let blocking = self.file(i, a[0] as i32).map_or(false, |f| {
                            let f = f.borrow();
                            let tcp = matches!(f.kind, Kind::Tcp { .. });
                            (tcp || (n == 145 && matches!(f.kind, Kind::Pipe { .. }))) && f.flags & O_NONBLOCK == 0
                        });
                        if blocking {
                            let fd = a[0] as i32;
                            return if n == 146 {
                                Block(Wait::Send { fd, buf: base, len }, None)
                            } else {
                                Block(Wait::Read { fd, buf: base, len }, self.recv_deadline(i, fd))
                            };
                        }
                    }
                    if r < 0 {
                        return Done(if total > 0 { total } else { r });
                    }
                    total += r;
                    if (r as u32) < len {
                        break;
                    }
                }
                Done(total)
            }
            180 | 181 => {
                let offset = u64::from(a[4]) | u64::from(a[5]) << 32;
                let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                let saved = f.borrow().offset;
                f.borrow_mut().offset = offset;
                let r = if n == 180 {
                    self.read_now(i, a[0] as i32, a[1], a[2])
                } else {
                    self.write(i, a[0] as i32, a[1], a[2], Some(offset))
                };
                f.borrow_mut().offset = saved;
                Done(r)
            }
            19 | 140 => {
                let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                let (off, whence) = if n == 19 {
                    (a[1] as i32 as i64, a[2])
                } else {
                    ((u64::from(a[1]) << 32 | u64::from(a[2])) as i64, a[4])
                };
                let size = {
                    let f = f.borrow();
                    match &f.kind {
                        Kind::File { path } => self.fs.contents(path).map_or(0, |c| c.len()) as i64,
                        Kind::Dir { .. } => 0,
                        _ => return Done(ESPIPE),
                    }
                };
                let mut f = f.borrow_mut();
                let new = match whence {
                    0 => off,
                    1 => f.offset as i64 + off,
                    2 => size + off,
                    _ => return Done(EINVAL),
                };
                if new < 0 {
                    return Done(EINVAL);
                }
                f.offset = new as u64;
                drop(f);
                if n == 140 {
                    let ok = self.wr(i, a[3], &(new as u64).to_le_bytes());
                    return Done(if ok { 0 } else { EFAULT });
                }
                Done(new.min(i32::MAX as i64) as i32)
            }
            41 | 63 | 358 => {
                let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                if n == 41 {
                    let app = self.apps[i].as_mut().unwrap();
                    let pos = app.fds.iter().position(|s| s.is_none()).unwrap_or(app.fds.len());
                    if pos == app.fds.len() {
                        app.fds.push(None);
                    }
                    app.fds[pos] = Some(f);
                    return Done(pos as i32);
                }
                let target = a[1] as usize;
                if target >= 1024 {
                    return Done(EBADF);
                }
                let app = self.apps[i].as_mut().unwrap();
                while app.fds.len() <= target {
                    app.fds.push(None);
                }
                app.fds[target] = Some(f);
                Done(target as i32)
            }
            55 | 221 => self.fcntl(i, a[0] as i32, a[1], a[2]),
            54 => self.ioctl(i, a[0] as i32, a[1], a[2]),
            195 | 196 => {
                let p = match self.at(i, AT_FDCWD, a[0]) { Ok(p) => p, Err(e) => return Done(e) };
                Done(self.stat_path(i, &p, a[1]))
            }
            197 => Done(self.fstat(i, a[0] as i32, a[1])),
            327 => {
                if a[3] & 0x1000 != 0 && self.rstr(i, a[1]).map_or(false, |s| s.is_empty()) {
                    return Done(self.fstat(i, a[0] as i32, a[2]));
                }
                let p = match self.at(i, a[0] as i32, a[1]) { Ok(p) => p, Err(e) => return Done(e) };
                Done(self.stat_path(i, &p, a[2]))
            }
            397 => Done(ENOSYS), // statx: C libraries fall back to fstatat64
            266 | 267 => {
                // statfs64: report a small in-memory file system.
                let mut b = [0u8; 84];
                b[..4].copy_from_slice(&0x01021994u32.to_le_bytes());
                b[4..8].copy_from_slice(&4096u32.to_le_bytes());
                self.wr(i, a[2], &b);
                Done(0)
            }
            33 | 334 | 439 => {
                let (dirfd, ptr) = if n == 33 { (AT_FDCWD, a[0]) } else { (a[0] as i32, a[1]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                Done(if self.fs.stat(&p).is_some() { 0 } else { ENOENT })
            }
            217 | 141 => self.getdents(i, a[0] as i32, a[1], a[2]),
            183 => {
                let mut cwd = self.apps[i].as_ref().unwrap().cwd.clone().into_bytes();
                cwd.push(0);
                if cwd.len() > a[1] as usize {
                    return Done(ERANGE);
                }
                let len = cwd.len() as i32;
                Done(if self.wr(i, a[0], &cwd) { len } else { EFAULT })
            }
            12 | 133 => {
                let p = if n == 12 {
                    match self.at(i, AT_FDCWD, a[0]) { Ok(p) => p, Err(e) => return Done(e) }
                } else {
                    let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                    let f = f.borrow();
                    match &f.kind { Kind::Dir { path, .. } => path.clone(), _ => return Done(ENOTDIR) }
                };
                match self.fs.stat(&p) {
                    Some(s) if s.mode & S_IFMT == S_IFDIR => {
                        self.apps[i].as_mut().unwrap().cwd = p;
                        Done(0)
                    }
                    Some(_) => Done(ENOTDIR),
                    None => Done(ENOENT),
                }
            }
            39 | 323 => {
                let (dirfd, ptr) = if n == 39 { (AT_FDCWD, a[0]) } else { (a[0] as i32, a[1]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                Done(match self.fs.mkdir(&p) { Ok(()) => 0, Err(e) => e })
            }
            40 | 10 | 328 => {
                let (dirfd, ptr, dir) = match n {
                    40 => (AT_FDCWD, a[0], true),
                    10 => (AT_FDCWD, a[0], false),
                    _ => (a[0] as i32, a[1], a[2] & 0x200 != 0),
                };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                if !super::radios::app_may_write(&p) {
                    return Done(EACCES);
                }
                Done(match self.fs.unlink(&p, dir) { Ok(()) => 0, Err(e) => e })
            }
            38 | 329 | 382 => {
                let (fa, fp, ta, tp) = if n == 38 { (AT_FDCWD, a[0], AT_FDCWD, a[1]) } else { (a[0] as i32, a[1], a[2] as i32, a[3]) };
                let from = match self.at(i, fa, fp) { Ok(p) => p, Err(e) => return Done(e) };
                let to = match self.at(i, ta, tp) { Ok(p) => p, Err(e) => return Done(e) };
                if !super::radios::app_may_write(&from) || !super::radios::app_may_write(&to) {
                    return Done(EACCES);
                }
                Done(match self.fs.rename(&from, &to) { Ok(()) => 0, Err(e) => e })
            }
            85 | 332 => {
                let (dirfd, ptr, buf, len) = if n == 85 { (AT_FDCWD, a[0], a[1], a[2]) } else { (a[0] as i32, a[1], a[2], a[3]) };
                let p = match self.at(i, dirfd, ptr) { Ok(p) => p, Err(e) => return Done(e) };
                if p == "/proc/self/exe" {
                    let exe = self.apps[i].as_ref().unwrap().path.clone();
                    let bytes = &exe.as_bytes()[..exe.len().min(len as usize)];
                    let count = bytes.len() as i32;
                    return Done(if self.wr(i, buf, bytes) { count } else { EFAULT });
                }
                Done(if self.fs.stat(&p).is_some() { EINVAL } else { ENOENT })
            }
            92 | 93 | 193 | 194 => {
                let len = match n {
                    92 | 93 => u64::from(a[1]),
                    _ => u64::from(a[2]) | u64::from(a[3]) << 32,
                };
                let path = if n == 93 || n == 194 {
                    let Some(f) = self.file(i, a[0] as i32) else { return Done(EBADF) };
                    let f = f.borrow();
                    match &f.kind { Kind::File { path } => path.clone(), _ => return Done(EINVAL) }
                } else {
                    match self.at(i, AT_FDCWD, a[0]) { Ok(p) => p, Err(e) => return Done(e) }
                };
                if !super::radios::app_may_write(&path) {
                    return Done(EACCES);
                }
                let now = (self.clock(0) / 1_000_000_000) as u32;
                match self.fs.writable(&path, false, false, now) {
                    Some(data) => {
                        data.resize(len as usize, 0);
                        Done(0)
                    }
                    None => Done(ENOENT),
                }
            }
            118 | 148 | 36 | 373 | 15 | 94 | 333 | 325 | 95 | 207 | 212 | 269 | 348 | 412 | 143 | 352 => Done(0),
            60 => Done(0o022),
            122 => {
                let mut u = [0u8; 390];
                for (k, field) in ["Linux", "violet", "6.1.0-violetos", "#1 VioletOS open runtime", "armv7l", "(none)"].iter().enumerate() {
                    u[k * 65..k * 65 + field.len()].copy_from_slice(field.as_bytes());
                }
                Done(if self.wr(i, a[0], &u) { 0 } else { EFAULT })
            }
            191 | 369 => {
                let (resource, old) = if n == 191 { (a[0], a[1]) } else { (a[1], a[3]) };
                if old != 0 {
                    let (cur, max): (u64, u64) = match resource {
                        3 => (8 << 20, u64::MAX),
                        7 => (1024, 4096),
                        _ => (u64::MAX, u64::MAX),
                    };
                    if n == 191 {
                        let mut b = [0u8; 8];
                        b[..4].copy_from_slice(&(cur.min(u32::MAX as u64) as u32).to_le_bytes());
                        b[4..].copy_from_slice(&(max.min(u32::MAX as u64) as u32).to_le_bytes());
                        self.wr(i, old, &b);
                    } else {
                        let mut b = [0u8; 16];
                        b[..8].copy_from_slice(&cur.to_le_bytes());
                        b[8..].copy_from_slice(&max.to_le_bytes());
                        self.wr(i, old, &b);
                    }
                }
                Done(0)
            }
            75 => Done(0),
            384 => {
                let mut v = Vec::with_capacity(a[1] as usize);
                while v.len() < a[1] as usize {
                    v.extend_from_slice(&self.next_random().to_le_bytes());
                }
                v.truncate(a[1] as usize);
                Done(if self.wr(i, a[0], &v) { a[1] as i32 } else { EFAULT })
            }
            // ---- waiting ----
            168 | 336 | 414 => {
                let timeout = match n {
                    168 => (a[2] as i32 >= 0).then(|| u64::from(a[2]) * 1_000_000),
                    _ => if a[2] == 0 { None } else { self.timespec(i, a[2], n == 414) },
                };
                self.poll(i, a[0], a[1], timeout)
            }
            142 | 335 | 413 => Done(ENOSYS),
            250 | 357 => Done(self.install(i, OpenFile::new(Kind::Epoll { interest: Vec::new(), reported: Vec::new() }, 2), 0)),
            251 => self.epoll_ctl(i, a[0] as i32, a[1], a[2] as i32, a[3]),
            252 | 346 => {
                let max = a[2];
                if max == 0 {
                    return Done(EINVAL);
                }
                let got = self.epoll_collect(i, a[0] as i32, a[1], max);
                if got != 0 || a[3] == 0 {
                    return Done(got);
                }
                let deadline = (a[3] as i32 > 0).then(|| sys::now_ns() + u64::from(a[3]) * 1_000_000);
                Block(Wait::Epoll { epfd: a[0] as i32, events: a[1], max }, deadline)
            }
            351 | 356 => Done(self.install(i, OpenFile::new(Kind::EventFd { count: u64::from(a[0]) }, 2 | a[1] & O_NONBLOCK), 0)),
            42 | 359 => {
                let buffer = Rc::new(RefCell::new(alloc::collections::VecDeque::new()));
                let open = Rc::new(RefCell::new(1u32));
                let flags = if n == 359 { a[1] } else { 0 };
                let r = self.install(i, OpenFile::new(Kind::Pipe { buffer: buffer.clone(), write: false, peer_open: open.clone() }, flags & O_NONBLOCK), 0);
                let w = self.install(i, OpenFile::new(Kind::Pipe { buffer, write: true, peer_open: open }, 1 | flags & O_NONBLOCK), 0);
                let mut b = [0u8; 8];
                b[..4].copy_from_slice(&r.to_le_bytes());
                b[4..].copy_from_slice(&w.to_le_bytes());
                Done(if self.wr(i, a[0], &b) { 0 } else { EFAULT })
            }
            350 | 355 | 360 | 385 => Done(ENOSYS),
            // ---- sockets: TCP through Radio, DNS answered here (sockets.rs) ----
            281..=297 | 366 => self.socket_call(i, n, a),
            374 | 365 => Done(ENOSYS),
            _ => {
                let app = self.apps[i].as_ref().unwrap();
                crate::log!("{}: unsupported system call {}", app.name, n);
                Done(ENOSYS)
            }
        }
    }

    fn exit_thread(&mut self, i: usize, slot: usize) -> Outcome {
        let (clear, tcb) = {
            let t = &self.apps[i].as_ref().unwrap().threads[slot];
            (t.clear_tid, t.tcb)
        };
        if clear != 0 {
            self.wr32(i, clear, 0);
            self.futex_wake(i, clear, 1);
        }
        sys::tcb_suspend(tcb);
        let app = self.apps[i].as_mut().unwrap();
        app.threads[slot].alive = false;
        if !app.threads.iter().any(|t| t.alive) {
            app.exit_code.get_or_insert(0);
            self.reap(i);
        }
        Gone
    }

    fn clone(&mut self, i: usize, slot: usize, a: [u32; 7], m: &[u32; 16]) -> Outcome {
        let (flags, stack, ptid, tls, ctid) = (a[0], a[1], a[2], a[3], a[4]);
        if flags & CLONE_VM == 0 {
            return Done(ENOSYS); // fork: one address space per app for now
        }
        let parent = self.apps[i].as_ref().unwrap().threads[slot].tcb;
        let Ok(mut regs) = sys::tcb_read(parent) else { return Done(EAGAIN) };
        // The fault message holds r0-r7 as the svc saw them.
        regs.r0 = 0;
        regs.r1 = m[1];
        regs.r2 = m[2];
        regs.r3 = m[3];
        regs.r4 = m[4];
        regs.r5 = m[5];
        regs.r6 = m[6];
        regs.r7 = m[7];
        regs.r14 = m[sys::MR_LR];
        regs.sp = if stack != 0 { stack } else { m[sys::MR_SP] };
        if flags & CLONE_SETTLS != 0 {
            regs.tpidruro = tls;
        }
        let thumb = (m[MR_CPSR] >> 5) & 1;
        let target = m[MR_FAULT_IP].wrapping_add(4) | thumb;
        let tid = self.next_tid();
        let child = match self.spawn(i, tid, regs, target) {
            Ok(c) => c,
            Err(_) => return Done(EAGAIN),
        };
        if flags & CLONE_PARENT_SETTID != 0 {
            self.wr32(i, ptid, tid);
        }
        if flags & CLONE_CHILD_SETTID != 0 {
            self.wr32(i, ctid, tid);
        }
        if flags & CLONE_CHILD_CLEARTID != 0 {
            self.apps[i].as_mut().unwrap().threads[child].clear_tid = ctid;
        }
        Done(tid as i32)
    }

    fn futex(&mut self, i: usize, a: [u32; 7], time64: bool) -> Outcome {
        let (addr, op, val, timeout) = (a[0], a[1] & 0x7f, a[2], a[3]);
        match op {
            0 | 9 => {
                let Some(current) = self.rd32(i, addr) else { return Done(EFAULT) };
                if current != val {
                    return Done(EAGAIN);
                }
                let deadline = if timeout == 0 {
                    None
                } else {
                    let Some(t) = self.timespec(i, timeout, time64) else { return Done(EFAULT) };
                    Some(if op == 9 {
                        let clock = if a[1] & 256 != 0 { 0 } else { 1 };
                        sys::now_ns() + t.saturating_sub(self.clock(clock))
                    } else {
                        sys::now_ns() + t
                    })
                };
                Block(Wait::Futex { app: i, addr }, deadline)
            }
            1 | 10 => Done(self.futex_wake(i, addr, val as usize) as i32),
            // Requeue: waking every waiter is a permitted spurious wakeup.
            3 | 4 => {
                let woken = self.futex_wake(i, addr, usize::MAX) + self.futex_wake(i, a[4], usize::MAX);
                Done(woken.min(val as usize) as i32)
            }
            5 => Done(self.futex_wake(i, addr, val as usize) as i32 + self.futex_wake(i, a[4], a[3] as usize) as i32),
            _ => Done(ENOSYS),
        }
    }

    pub fn futex_wake(&mut self, i: usize, addr: u32, count: usize) -> usize {
        let waiting: Vec<usize> = self.apps[i]
            .as_ref()
            .map(|app| {
                app.threads
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| matches!(t.wait, Some(Wait::Futex { addr: w, .. }) if w == addr))
                    .map(|(s, _)| s)
                    .take(count)
                    .collect()
            })
            .unwrap_or_default();
        for &s in &waiting {
            self.wake(i, s, 0);
        }
        waiting.len()
    }

    fn next_tid(&mut self) -> u32 {
        let t = self.next_pid_bump();
        t
    }

    fn open(&mut self, i: usize, dirfd: i32, path_ptr: u32, flags: u32) -> Outcome {
        let path = match self.at(i, dirfd, path_ptr) {
            Ok(p) => p,
            Err(e) => return Done(e),
        };
        let mode = if flags & O_ACCMODE == 0 { AccessMode::Read } else { AccessMode::Write };
        let app_id = AppId(self.apps[i].as_ref().unwrap().pid);
        if self.policy.check(app_id, &Action::OpenPath { path: &path, mode }) != Decision::Allow {
            return Done(EACCES);
        }
        // Violet Shield sees the same private copy of the path that is opened below.
        match self.gate(i, &Action::OpenPath { path: &path, mode }) {
            super::Gate::Allow => {}
            super::Gate::Deny => return Done(EACCES),
            super::Gate::Frozen => return Block(Wait::Frozen, None),
            super::Gate::Killed => return Gone,
        }
        let kind = match path.as_str() {
            "/dev/violet/screen" => {
                if let Err(e) = self.surface(i) {
                    return Done(e);
                }
                Kind::Screen
            }
            "/dev/violet/events" => Kind::Events,
            "/dev/violet/sys" => Kind::Sys { pending: Vec::new() },
            "/dev/violet/audio" => {
                if !super::device::audio_present(self.audio) {
                    return Done(ENODEV);
                }
                Kind::Audio { channels: 2, pending: Vec::new() }
            }
            "/dev/violet/camera" => {
                if !super::device::camera::present(self.camera) {
                    return Done(ENODEV);
                }
                if self.focused && self.foreground == Some(i) {
                    super::device::camera::want(true);
                }
                // Only frames that complete after the open.
                Kind::Camera { seen: super::device::camera::frames() }
            }
            "/dev/violet/mic" => {
                if !super::device::audio_present(self.audio) {
                    return Done(ENODEV);
                }
                Kind::Mic { seen: super::device::mic::open() }
            }
            "/dev/violet/hci" => {
                if super::hci::blocked() {
                    return Done(EACCES);
                }
                if !super::hci::open() {
                    return Done(EBUSY);
                }
                Kind::Hci
            }
            "/dev/null" => Kind::Null,
            "/dev/urandom" | "/dev/random" => Kind::Random,
            "/dev/tty" | "/dev/stdout" | "/dev/stderr" | "/dev/console" => Kind::Log,
            _ => {
                let now = (self.clock(0) / 1_000_000_000) as u32;
                match self.fs.stat(&path) {
                    Some(s) if flags & O_CREAT != 0 && flags & O_EXCL != 0 => {
                        let _ = s;
                        return Done(EEXIST);
                    }
                    Some(s) if s.mode & S_IFMT == S_IFDIR => {
                        if flags & O_ACCMODE != 0 {
                            return Done(EISDIR);
                        }
                        Kind::Dir { entries: self.fs.list(&path), path, pos: 0 }
                    }
                    Some(_) if flags & O_DIRECTORY != 0 => return Done(ENOTDIR),
                    Some(s) if s.mode & S_IFMT == S_IFCHR => Kind::Null,
                    existing => {
                        let wants_write = flags & O_ACCMODE != 0 || flags & O_TRUNC != 0;
                        if existing.is_none() && flags & O_CREAT == 0 {
                            return Done(ENOENT);
                        }
                        if (wants_write || existing.is_none()) && !super::radios::app_may_write(&path) {
                            return Done(EACCES);
                        }
                        if wants_write || existing.is_none() {
                            if self.fs.writable(&path, flags & O_CREAT != 0, flags & O_TRUNC != 0, now).is_none() {
                                return Done(ENOENT);
                            }
                        }
                        Kind::File { path }
                    }
                }
            }
        };
        Done(self.install(i, OpenFile::new(kind, flags), 0))
    }

    fn read(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> Outcome {
        let r = self.read_now(i, fd, buf, len);
        if r != EAGAIN {
            return Done(r);
        }
        let nonblock = self.file(i, fd).map_or(true, |f| f.borrow().flags & O_NONBLOCK != 0);
        if nonblock {
            return Done(EAGAIN);
        }
        Block(Wait::Read { fd, buf, len }, self.recv_deadline(i, fd))
    }

    /// When a blocking read of fd gives up (its SO_RCVTIMEO), if ever.
    pub fn recv_deadline(&self, i: usize, fd: i32) -> Option<u64> {
        let t = self.file(i, fd)?.borrow().recv_timeout;
        (t > 0).then(|| sys::now_ns() + t)
    }

    pub(crate) fn write(&mut self, i: usize, fd: i32, buf: u32, len: u32, at: Option<u64>) -> i32 {
        let Some(f) = self.file(i, fd) else { return EBADF };
        if matches!(f.borrow().kind, Kind::Audio { .. }) {
            let r = self.write_audio(i, fd, buf, len);
            return if r == 0 { EAGAIN } else { r };
        }
        if matches!(f.borrow().kind, Kind::Tcp { .. } | Kind::Udp { .. }) {
            return self.socket_send(i, fd, buf, len, None);
        }
        let Some(data) = self.rd(i, buf, len as usize) else { return EFAULT };
        let mut f = f.borrow_mut();
        let flags = f.flags;
        match &mut f.kind {
            Kind::Log => {
                Log::write(&data);
                len as i32
            }
            Kind::Null | Kind::Random => len as i32,
            Kind::Hci => {
                if !super::hci::up() {
                    ENETDOWN
                } else if !super::hci::valid(&data) {
                    EINVAL
                } else if super::hci::send(&data) {
                    len as i32
                } else {
                    EAGAIN
                }
            }
            Kind::File { path } => {
                let path = path.clone();
                let now = (self.clock(0) / 1_000_000_000) as u32;
                let Some(content) = self.fs.writable(&path, false, false, now) else { return EIO };
                let start = if flags & O_APPEND != 0 { content.len() } else { at.unwrap_or(f.offset) as usize };
                if content.len() < start + data.len() {
                    content.resize(start + data.len(), 0);
                }
                content[start..start + data.len()].copy_from_slice(&data);
                if at.is_none() {
                    f.offset = (start + data.len()) as u64;
                }
                len as i32
            }
            Kind::Screen => {
                if data.len() < 24 {
                    return EINVAL;
                }
                let word = |k: usize| u32::from_le_bytes(data[k * 4..k * 4 + 4].try_into().unwrap());
                let rect = PresentRect { x: word(0), y: word(1), width: word(2), height: word(3), buffer_index: word(4), flags: word(5) };
                drop(f);
                self.present(i, rect);
                len as i32
            }
            Kind::Sys { .. } => {
                let text = String::from_utf8_lossy(&data).into_owned();
                drop(f);
                let mut answer = String::new();
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    answer.push_str(&self.system(i, line));
                }
                if let Some(f) = self.file(i, fd) {
                    if let Kind::Sys { pending } = &mut f.borrow_mut().kind {
                        pending.extend_from_slice(answer.as_bytes());
                    }
                }
                len as i32
            }
            Kind::EventFd { count } => {
                if data.len() < 8 {
                    return EINVAL;
                }
                *count = count.saturating_add(u64::from_le_bytes(data[..8].try_into().unwrap()));
                8
            }
            Kind::Pipe { buffer, write: true, .. } => {
                buffer.borrow_mut().extend(data.iter().copied());
                len as i32
            }
            _ => EBADF,
        }
    }

    fn fcntl(&mut self, i: usize, fd: i32, cmd: u32, arg: u32) -> Outcome {
        let Some(f) = self.file(i, fd) else { return Done(EBADF) };
        match cmd {
            0 | 1030 => Done(self.install_ref(i, f, arg as usize)),
            1 | 2 => Done(0),
            3 => Done((f.borrow().flags | 0o400000) as i32),
            4 => {
                let mut f = f.borrow_mut();
                f.flags = (f.flags & !(O_NONBLOCK | O_APPEND)) | (arg & (O_NONBLOCK | O_APPEND));
                Done(0)
            }
            5 | 6 | 7 | 12 | 13 | 14 | 36 | 37 | 38 => Done(0),
            _ => Done(EINVAL),
        }
    }
    fn install_ref(&mut self, i: usize, f: FileRef, from: usize) -> i32 {
        let app = self.apps[i].as_mut().unwrap();
        while app.fds.len() <= from {
            app.fds.push(None);
        }
        let pos = (from..app.fds.len()).find(|&k| app.fds[k].is_none()).unwrap_or(app.fds.len());
        if pos == app.fds.len() {
            app.fds.push(None);
        }
        app.fds[pos] = Some(f);
        pos as i32
    }

    fn ioctl(&mut self, i: usize, fd: i32, cmd: u32, arg: u32) -> Outcome {
        let Some(f) = self.file(i, fd) else { return Done(EBADF) };
        let is_screen = matches!(f.borrow().kind, Kind::Screen);
        match cmd {
            0x5601 if is_screen => {
                let info = [WIDTH as u32, HEIGHT as u32, STRIDE as u32, PIXEL_FORMAT_BGRA8888, 2, BUFFER_BYTES as u32];
                let mut b = [0u8; 24];
                for (k, w) in info.iter().enumerate() {
                    b[k * 4..k * 4 + 4].copy_from_slice(&w.to_le_bytes());
                }
                Done(if self.wr(i, arg, &b) { 0 } else { EFAULT })
            }
            // SCREEN_IOC_WAIT_VSYNC: until the next frame starts; VsyncInfo (16 bytes) to arg if not 0.
            0x5604 if is_screen => {
                let deadline = super::sys::now_ns() + 100_000_000;
                Block(Wait::Vsync { after: self.beam.frame, ptr: arg }, Some(deadline))
            }
            // AUDIO_IOC_CONFIG: 48 kHz s16, one or two channels.
            0x5610 => {
                let Some(b) = self.rd(i, arg, 16) else { return Done(EFAULT) };
                let word = |k: usize| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap());
                let (rate, channels, format) = (word(0), word(1), word(2));
                if rate != 48000 || !(1..=2).contains(&channels) || format != AUDIO_FMT_S16LE {
                    return Done(EINVAL);
                }
                match &mut f.borrow_mut().kind {
                    Kind::Audio { channels: c, pending } => {
                        *c = channels;
                        pending.clear();
                        Done(0)
                    }
                    _ => Done(ENOTTY),
                }
            }
            0x5421 => {
                let on = self.rd32(i, arg).unwrap_or(0) != 0;
                let mut f = f.borrow_mut();
                if on { f.flags |= O_NONBLOCK } else { f.flags &= !O_NONBLOCK }
                Done(0)
            }
            0x541b => {
                let n = match &f.borrow().kind {
                    Kind::Pipe { buffer, .. } => buffer.borrow().len() as u32,
                    _ => 0,
                };
                self.wr32(i, arg, n);
                Done(0)
            }
            _ => Done(ENOTTY),
        }
    }

    fn mmap(&mut self, i: usize, addr: u32, len: u32, prot: u32, flags: u32, fd: i32, offset: u32) -> Outcome {
        if len == 0 || len > 0x8000_0000 {
            return Done(EINVAL);
        }
        if prot & 6 == 6 {
            match self.gate(i, &Action::MapWriteExec { addr, len }) {
                super::Gate::Allow => {}
                super::Gate::Deny => return Done(EACCES),
                super::Gate::Frozen => return Block(Wait::Frozen, None),
                super::Gate::Killed => return Gone,
            }
        }
        let len = align_up(len, 4096);
        let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
        let screen = flags & MAP_ANONYMOUS == 0
            && self.file(i, fd).map_or(false, |f| matches!(f.borrow().kind, Kind::Screen));
        let app = self.apps[i].as_mut().unwrap();
        let start = if fixed {
            if addr & 0xfff != 0 || addr.checked_add(len).map_or(true, |e| e > USER_TOP) {
                return Done(EINVAL);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && app.space.overlaps(addr, addr + len) {
                return Done(EEXIST);
            }
            app.space.remove(&mut self.k, &mut self.pool, addr, addr + len);
            addr
        } else if addr != 0 && addr & 0xffff == 0 && addr >= 0x1000_0000
            && addr.checked_add(len).map_or(false, |e| e <= super::procmgr::MMAP_TOP)
            && !app.space.overlaps(addr, addr + len)
        {
            addr
        } else {
            match app.space.free_range(len) {
                Some(a) => a,
                None => return Done(ENOMEM),
            }
        };
        app.space.add(start, start + len, prot & 7);
        if screen {
            if start & 0xffff != 0 || offset as usize % sys::PAGE != 0 {
                return Done(EINVAL);
            }
            if let Err(e) = self.map_surface(i, start, len, offset) {
                return Done(e);
            }
            return Done(start as i32);
        }
        if flags & MAP_ANONYMOUS == 0 {
            let Some(f) = self.file(i, fd) else { return Done(EBADF) };
            let path = match &f.borrow().kind {
                Kind::File { path } => path.clone(),
                _ => return Done(ENODEV),
            };
            let _ = MAP_SHARED; // Shared file mappings are not written back.
            let data = self.fs.contents(&path).map(|c| {
                let s = (offset as usize).min(c.len());
                let e = (s + len as usize).min(c.len());
                c[s..e].to_vec()
            });
            if let Some(data) = data {
                let app = self.apps[i].as_mut().unwrap();
                let saved = app.space.find(start).map(|v| v.prot);
                // Write the file in even if the mapping is read-only.
                app.space.add(start, start + len, PROT_READ | PROT_WRITE);
                let mut ok = app.space.write(&mut self.k, &mut self.pool, start, &data);
                // Out of pages: close a background app and try again.
                while !ok && self.foreground == Some(i) && self.pool.available() == 0 && self.reclaim(None) {
                    ok = self.apps[i].as_mut().unwrap().space.write(&mut self.k, &mut self.pool, start, &data);
                }
                let app = self.apps[i].as_mut().unwrap();
                app.space.add(start, start + len, saved.unwrap_or(prot & 7));
                if prot & 4 != 0 {
                    app.space.sync_code(start, start + len);
                }
                if !ok {
                    return Done(ENOMEM);
                }
            }
        }
        Done(start as i32)
    }

    fn put_stat(&mut self, i: usize, st: fs::Stat, buf: u32) -> i32 {
        let mut b = [0u8; 104];
        b[..8].copy_from_slice(&1u64.to_le_bytes());
        b[12..16].copy_from_slice(&(st.inode as u32).to_le_bytes());
        b[16..20].copy_from_slice(&st.mode.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[48..56].copy_from_slice(&st.size.to_le_bytes());
        b[56..60].copy_from_slice(&4096u32.to_le_bytes());
        b[64..72].copy_from_slice(&((st.size + 511) / 512).to_le_bytes());
        for at in [72usize, 80, 88] {
            b[at..at + 4].copy_from_slice(&st.mtime.to_le_bytes());
        }
        b[96..104].copy_from_slice(&st.inode.to_le_bytes());
        if self.wr(i, buf, &b) { 0 } else { EFAULT }
    }
    fn stat_path(&mut self, i: usize, path: &str, buf: u32) -> i32 {
        match self.fs.stat(path) {
            Some(st) => self.put_stat(i, st, buf),
            None => ENOENT,
        }
    }
    fn fstat(&mut self, i: usize, fd: i32, buf: u32) -> i32 {
        let Some(f) = self.file(i, fd) else { return EBADF };
        let st = match &f.borrow().kind {
            Kind::File { path } | Kind::Dir { path, .. } => self.fs.stat(path),
            _ => Some(fs::Stat { mode: S_IFCHR | 0o666, size: 0, mtime: 0, inode: fd as u64 + 1 }),
        };
        match st {
            Some(st) => self.put_stat(i, st, buf),
            None => ENOENT,
        }
    }

    fn getdents(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> Outcome {
        let Some(f) = self.file(i, fd) else { return Done(EBADF) };
        let mut out = Vec::new();
        {
            let mut f = f.borrow_mut();
            let Kind::Dir { entries, pos, .. } = &mut f.kind else { return Done(ENOTDIR) };
            // "." and ".." first, then the entries.
            while *pos < entries.len() + 2 {
                let (name, dir) = match *pos {
                    0 => (String::from("."), true),
                    1 => (String::from(".."), true),
                    k => entries[k - 2].clone(),
                };
                let reclen = (19 + name.len() + 1 + 7) & !7;
                if out.len() + reclen > len as usize {
                    break;
                }
                let start = out.len();
                out.resize(start + reclen, 0);
                let ino = (*pos as u64) + 1;
                out[start..start + 8].copy_from_slice(&ino.to_le_bytes());
                out[start + 8..start + 16].copy_from_slice(&((*pos as u64) + 1).to_le_bytes());
                out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
                out[start + 18] = if dir { 4 } else { 8 };
                out[start + 19..start + 19 + name.len()].copy_from_slice(name.as_bytes());
                *pos += 1;
            }
            if out.is_empty() && *pos < entries.len() + 2 {
                return Done(EINVAL);
            }
        }
        let n = out.len() as i32;
        Done(if self.wr(i, buf, &out) { n } else { EFAULT })
    }

    fn poll(&mut self, i: usize, ptr: u32, nfds: u32, timeout: Option<u64>) -> Outcome {
        let mut fds = Vec::new();
        for k in 0..nfds.min(1024) {
            let Some(b) = self.rd(i, ptr + 8 * k, 8) else { return Done(EFAULT) };
            let fd = i32::from_le_bytes(b[..4].try_into().unwrap());
            let events = i16::from_le_bytes(b[4..6].try_into().unwrap());
            fds.push((fd, events));
        }
        let wait = Wait::Poll { fds, ptr, time64: false };
        if let Some(n) = self.ready(i, &wait) {
            return Done(n);
        }
        // Clear revents for a timeout of zero or a later wake.
        if let Wait::Poll { fds, .. } = &wait {
            for k in 0..fds.len() {
                self.wr(i, ptr + 8 * k as u32 + 6, &[0, 0]);
            }
        }
        match timeout {
            Some(0) => Done(0),
            Some(t) => Block(wait, Some(sys::now_ns() + t)),
            None => Block(wait, None),
        }
    }

    fn epoll_ctl(&mut self, i: usize, epfd: i32, op: u32, fd: i32, event: u32) -> Outcome {
        let (mask, data) = if op == 2 {
            (0, 0)
        } else {
            let Some(b) = self.rd(i, event, 16) else { return Done(EFAULT) };
            (u32::from_le_bytes(b[..4].try_into().unwrap()), u64::from_le_bytes(b[8..16].try_into().unwrap()))
        };
        if self.file(i, fd).is_none() {
            return Done(EBADF);
        }
        let Some(f) = self.file(i, epfd) else { return Done(EBADF) };
        let mut f = f.borrow_mut();
        let Kind::Epoll { interest, reported } = &mut f.kind else { return Done(EINVAL) };
        // Adding, changing or removing an entry re-arms its edge.
        reported.retain(|r| r.0 != fd);
        let existing = interest.iter().position(|e| e.0 == fd);
        match (op, existing) {
            (1, Some(_)) => Done(EEXIST),
            (1, None) => {
                interest.push((fd, mask, data));
                Done(0)
            }
            (2, Some(k)) => {
                interest.remove(k);
                Done(0)
            }
            (3, Some(k)) => {
                interest[k] = (fd, mask, data);
                Done(0)
            }
            _ => Done(ENOENT),
        }
    }
}
