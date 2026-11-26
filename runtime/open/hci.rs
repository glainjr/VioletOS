//! /dev/violet/hci: the Bluetooth controller's HCI for one open app at a time,
//! through two pages shared with Radio on the handset (targets/moto-g-pure/
//! bt.rs) or with the emulator's virtual controller in QEMU
//! (tools/bluetooth_controller.py). The layout is described in bt.rs.
//!
//! Each write is one H4 packet, type byte first: a command (0x01) or ACL
//! data (0x02). Each read returns one H4 packet from the controller: an event
//! (0x04) or ACL data (0x02). Both give ENETDOWN while the controller is not
//! on (on the handset it runs while Radio runs the connectivity chip).
use core::sync::atomic::{fence, AtomicBool, Ordering};

const BASE: usize = 0x0944_0000;
const TX: usize = BASE + 2048;
const TX_SIZE: u32 = 2048;
const RX: usize = BASE + 4096;
const RX_SIZE: u32 = 4096;
const MAGIC: u32 = 0x3149_4348;
const ON: u32 = 2;

fn get(i: usize) -> u32 {
    unsafe { (BASE as *const u32).add(i).read_volatile() }
}
fn set(i: usize, v: u32) {
    unsafe { (BASE as *mut u32).add(i).write_volatile(v) }
}
fn rx_byte(at: u32) -> u8 {
    unsafe { ((RX + (at % RX_SIZE) as usize) as *const u8).read_volatile() }
}

static OPEN: AtomicBool = AtomicBool::new(false);
/// Settings › Radios turned Bluetooth off (radios.rs).
static BLOCKED: AtomicBool = AtomicBool::new(false);

/// The controller is on and Bluetooth is not turned off in Settings › Radios.
pub fn up() -> bool {
    !blocked() && get(0) == MAGIC && get(1) == ON
}
pub fn blocked() -> bool {
    BLOCKED.load(Ordering::Relaxed)
}
/// Settings › Radios turns Bluetooth off or on. Off resets the controller, so
/// it stops anything an app had it doing, and no app may open it meanwhile.
pub fn set_blocked(off: bool) {
    if off && !blocked() && get(0) == MAGIC && get(1) == ON {
        send(&[1, 0x03, 0x0c, 0]);
    }
    BLOCKED.store(off, Ordering::Relaxed);
}
/// Claims the controller. Events that arrived before are dropped.
/// Whether an app has /dev/violet/hci open.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

pub fn open() -> bool {
    if OPEN.swap(true, Ordering::Relaxed) {
        return false;
    }
    set(3, 1);
    set(7, get(6));
    true
}
pub fn close() {
    set(3, 0);
    OPEN.store(false, Ordering::Relaxed);
}
/// The length of the complete packet at the front of the event ring, if any.
/// A byte that starts no packet is skipped, so the stream resynchronizes.
fn next() -> Option<u32> {
    loop {
        let head = get(6);
        let tail = get(7);
        let have = head.wrapping_sub(tail);
        if have > RX_SIZE {
            // Radio restarted the controller: start again from what is there.
            set(7, head);
            return None;
        }
        if have == 0 {
            return None;
        }
        let need = match rx_byte(tail) {
            4 if have >= 3 => 3 + rx_byte(tail + 2) as u32,
            2 if have >= 5 => 5 + u32::from(u16::from_le_bytes([rx_byte(tail + 3), rx_byte(tail + 4)])),
            4 | 2 => return None,
            _ => {
                set(7, tail.wrapping_add(1));
                continue;
            }
        };
        return (have >= need).then_some(need);
    }
}
pub fn readable() -> bool {
    up() && next().is_some()
}
/// One packet from the controller, if a whole one has arrived.
pub fn take(max: usize) -> Option<alloc::vec::Vec<u8>> {
    let n = next()?;
    fence(Ordering::Acquire);
    let tail = get(7);
    let v: alloc::vec::Vec<u8> = (0..n.min(max as u32)).map(|k| rx_byte(tail.wrapping_add(k))).collect();
    set(7, tail.wrapping_add(n));
    Some(v)
}
pub fn space() -> u32 {
    TX_SIZE - get(4).wrapping_sub(get(5)).min(TX_SIZE)
}
/// Checks that `p` is one whole command or ACL packet.
pub fn valid(p: &[u8]) -> bool {
    match p.first() {
        Some(1) => p.len() >= 4 && p.len() == 4 + p[3] as usize,
        Some(2) => p.len() >= 5 && p.len() == 5 + u16::from_le_bytes([p[3], p[4]]) as usize && p.len() <= 1028,
        _ => false,
    }
}
/// Queues one packet; false when the ring has no room for it now.
pub fn send(p: &[u8]) -> bool {
    if space() < p.len() as u32 {
        return false;
    }
    let mut head = get(4);
    for &b in p {
        unsafe { ((TX + (head % TX_SIZE) as usize) as *mut u8).write_volatile(b) };
        head = head.wrapping_add(1);
    }
    fence(Ordering::Release);
    set(4, head);
    true
}
