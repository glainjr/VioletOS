//! Test-only MMIO and DMA device. The production WMT and AXI implementations
//! drive this model through the same byte, register, and clock operations.
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
};
const DMA: usize = 0x09c00000;
const PA: u32 = 0x42400000;
const REG: usize = 0x09011000;
const BT: usize = 0x09010000;
#[derive(Default)]
pub struct Model {
    regs: HashMap<usize, u32>,
    bytes: HashMap<usize, u8>,
    pub bt_tx: Vec<u8>,
    pub bt_rx: VecDeque<u8>,
    time: u32,
    rx_prod: [usize; 2],
    pub commands: Vec<Vec<u8>>,
    pub ciphertext: Vec<u8>,
    pub no_dma_completion: bool,
    pub drop_scan_done: bool,
    pub no_ack: bool,
    pub invalid_mac: bool,
    pub fixed_mac: Option<[u8; 6]>,
    pub deny_association: bool,
    pub wrong_channel: bool,
    pub drop_key_done: bool,
    pub wrong_seq: bool,
}
thread_local! {static STATE:RefCell<Model>=RefCell::new(Model::default());}
pub fn reset() {
    with(|m| *m = Model::default());
}
pub fn with<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}
impl Model {
    fn get(&self, a: usize) -> u32 {
        *self.regs.get(&a).unwrap_or(&0)
    }
    fn inject(&mut self, r: usize, bytes: &[u8]) {
        let reg = REG + 0x400 + r * 16;
        let slot = self.rx_prod[r];
        assert_eq!(self.get(reg + 4), 16);
        let desc = DMA + (self.get(reg) - PA) as usize + 16 * slot;
        assert_eq!(self.get(desc + 4) >> 31, 0);
        let buf = DMA + (self.get(desc) - PA) as usize;
        for (i, b) in bytes.iter().enumerate() {
            self.bytes.insert(buf + i, *b);
        }
        self.regs
            .insert(desc + 4, 0xc0000000 | ((bytes.len() as u32) << 16));
        self.rx_prod[r] = (slot + 1) % 16;
    }
    fn tx(&mut self, reg: usize, next: u32) {
        if self.no_dma_completion {
            return;
        }
        let slot = (next as usize + 15) % 16;
        let desc = DMA + (self.get(reg) - PA) as usize + slot * 16;
        let info = self.get(desc + 4);
        assert_eq!(info >> 30, 1);
        let n = ((info >> 16) & 0x3fff) as usize;
        let buf = DMA + (self.get(desc) - PA) as usize;
        let bytes: Vec<_> = (0..n).map(|i| self.bytes[&(buf + i)]).collect();
        assert_eq!(n, u16::from_le_bytes([bytes[0], bytes[1]]) as usize);
        self.regs.insert(desc + 4, info | 0x80000000);
        self.regs.insert(reg + 12, next);
        if reg == REG + 0x330 {
            if bytes[7] == 3 {
                self.ciphertext.extend_from_slice(&bytes[64..]);
                return;
            }
            assert_eq!(bytes[7], 2);
            let mut reply = [0; 28];
            reply[..4].copy_from_slice(&[28, 0, 0, 224]);
            reply[16..22].copy_from_slice(&[12, 0, 0, 224, 1, bytes[39]]);
            if self.wrong_seq {
                reply[21] ^= 1;
            }
            if !self.no_ack {
                self.inject(1, &reply);
            }
            if bytes[36] == 2 {
                self.regs.insert(0x09013140, 6);
            }
        } else if bytes[5] == 0xcc {
            let request = &bytes[32..];
            let auth = request[0] == 0xb0;
            assert!(auth || request[0] == 0);
            assert_eq!(&request[4..10], &request[16..22]);
            if auth {
                assert_eq!(&request[24..], &[0, 0, 1, 0, 0, 0]);
            } else {
                assert!(request.ends_with(super::station::RSN));
            }
            let mut packet = [0u8; 46];
            packet[..4].copy_from_slice(&[46, 0, 1, 224]);
            let frame = &mut packet[16..];
            frame[0] = if auth { 0xb0 } else { 0x10 };
            frame[4..10].copy_from_slice(&request[10..16]);
            frame[10..16].copy_from_slice(&request[4..10]);
            frame[16..22].copy_from_slice(&request[16..22]);
            if auth {
                frame[26] = 2;
            } else if self.deny_association {
                frame[26] = 17;
            } else {
                frame[28..30].copy_from_slice(&0xc003u16.to_le_bytes());
            }
            self.inject(0, &packet);
        } else if bytes[36] == 0x1c {
            let body = &bytes[64..];
            assert_eq!(body.len(), 24);
            if body[2] == 0 {
                let mut packet = [0; 52];
                packet[..4].copy_from_slice(&[52, 0, 0, 224]);
                packet[16..21].copy_from_slice(&[36, 0, 0, 224, 16]);
                packet[28..52].copy_from_slice(body);
                if self.wrong_channel {
                    packet[31] ^= 1;
                }
                self.inject(1, &packet);
            }
        } else if bytes[5] == 0x90 {
            assert_eq!(&bytes[2..6], &[8, 0x84, 1, 0x90]);
            assert_eq!(bytes[7], 2);
        } else if bytes[36] == 7 {
            assert_eq!(bytes.len(), 128);
            assert_eq!(bytes[75], 4);
            if bytes[66] == 1 && !self.drop_key_done {
                let mut packet = [0; 36];
                packet[..4].copy_from_slice(&[36, 0, 0, 224]);
                packet[16..21].copy_from_slice(&[20, 0, 0, 224, 0x24]);
                packet[30..36].copy_from_slice(&bytes[68..74]);
                self.inject(1, &packet);
            }
        } else if bytes[36] == 0x13 {
            let mut packet = [0; 36];
            packet[..4].copy_from_slice(&[36, 0, 0, 224]);
            packet[16..21].copy_from_slice(&[20, 0, 0, 224, 12]);
            packet[28..34].copy_from_slice(&bytes[66..72]);
            self.inject(1, &packet);
        } else if bytes[36] == 0x80 {
            assert_eq!(bytes.len(), 64 + 116);
            assert_eq!(bytes[38], 0);
            let mut reply = [0; 16 + 12 + 116];
            reply[..4].copy_from_slice(&[144, 0, 0, 224]);
            reply[16..22].copy_from_slice(&[128, 0, 0, 224, 1, bytes[39]]);
            if !self.invalid_mac {
                reply[36..42].copy_from_slice(&[2, 17, 34, 51, 68, 85]);
            }
            self.inject(1, &reply);
        } else if bytes[36] == 0x11 {
            let expected = self.fixed_mac.unwrap_or(if self.invalid_mac { [0xfe, 1, 2, 3, 4, 5] } else { [2, 17, 34, 51, 68, 85] });
            assert_eq!(bytes[68..74], expected);
        } else if bytes[36] == 3 {
            assert_eq!(bytes.len(), 1252);
            assert_eq!(bytes[66], 0);
            assert_eq!(bytes[69], 0);
            let mut beacon = [0u8; 62];
            beacon[..4].copy_from_slice(&[62, 0, 1, 224]);
            beacon[16] = 128;
            beacon[52..].copy_from_slice(b"\0\x08test-net");
            self.inject(0, &beacon);
            let mut done = [0u8; 36];
            done[..4].copy_from_slice(&[36, 0, 0, 224]);
            done[16..21].copy_from_slice(&[20, 0, 0, 224, 13]);
            done[28] = bytes[64];
            done[32] = bytes[64 + 159];
            done[33] = 7;
            if !self.drop_scan_done {
                self.inject(1, &done);
            }
        }
        self.commands.push(bytes);
    }
}
pub fn rd(a: usize) -> u32 {
    with(|m| {
        if a == BT + 0x14 {
            0x60 | u32::from(!m.bt_rx.is_empty())
        } else {
            m.get(a)
        }
    })
}
pub fn wr(a: usize, v: u32) {
    with(|m| {
        m.regs.insert(a, v);
        if (a == REG + 0x338 || a == REG + 0x3f8) && m.get(REG + 0x208) & 1 != 0 {
            m.tx(a - 8, v);
        }
    });
}
pub fn byte(a: usize) -> u8 {
    with(|m| {
        if a == BT {
            m.bt_rx.pop_front().unwrap()
        } else {
            m.bytes[&a]
        }
    })
}
pub fn put(a: usize, b: u8) {
    with(|m| {
        if a == BT {
            m.bt_tx.push(b)
        } else {
            m.bytes.insert(a, b);
        }
    });
}
pub fn fixed_address() -> Option<[u8; 6]> {
    with(|m| m.fixed_mac)
}
pub fn address_entropy(mac: &mut [u8; 6]) -> bool {
    *mac = [0xff, 1, 2, 3, 4, 5];
    true
}
pub fn barrier() {}
pub fn now() -> u32 {
    with(|m| {
        m.time += 1;
        m.time
    })
}
pub fn expired(start: u32, ms: u32) -> bool {
    now().wrapping_sub(start) >= ms
}

pub fn trace(_slot: usize, _value: u32) {}
