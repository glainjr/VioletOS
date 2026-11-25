//! Hardware access shared by the two sides of the MT6765 radio.
use crate::protocol::{ticks, ticks_per_ms, Message, GET, OK};
pub fn rd(addr: usize) -> u32 {
    unsafe { (addr as *const u32).read_volatile() }
}
pub fn wr(addr: usize, v: u32) {
    unsafe {
        (addr as *mut u32).write_volatile(v);
    }
    barrier();
}
pub fn byte(addr: usize) -> u8 {
    unsafe { (addr as *const u8).read_volatile() }
}
pub fn put(addr: usize, v: u8) {
    unsafe { (addr as *mut u8).write_volatile(v) }
}
pub fn barrier() {
    unsafe {
        core::arch::asm!("dsb sy");
    }
}
pub fn now() -> u32 {
    unsafe { ticks() }
}
pub fn expired(start: u32, ms: u32) -> bool {
    now().wrapping_sub(start) >= unsafe { ticks_per_ms } * ms
}

// Radio and USB share only this telemetry page. Keep data from the first failing transaction.
pub fn trace(slot: usize, value: u32) {
    unsafe {
        (0x09200000 as *mut u32)
            .add(35 + slot)
            .write_volatile(value)
    }
}

/// The station address provisioned over USB for this boot (moto-wifi-profile
/// with VIOLET_FIXED_MAC). Without one the address stays as it was: the chip's,
/// or a random one for each session.
pub fn fixed_address() -> Option<[u8; 6]> {
    let r = Message::new(GET, 0x4061, 0).call(7);
    let mac: [u8; 6] = r.data[..6].try_into().unwrap();
    (r.op == OK && mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

pub fn address_entropy(mac: &mut [u8; 6]) -> bool {
    unsafe { super::wpa::radio_random(mac.as_mut_ptr(), mac.len()) == 0 }
}
