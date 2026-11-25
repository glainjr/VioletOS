//! Full-speed, endpoint-zero-only status device for the developer handset.
use crate::protocol::{delay_ms, ticks, ticks_per_ms, Message, EXECUTE, OK};
const USB: usize = 0x09000000;
const PHY: usize = 0x09001800;
const CSR: usize = 0x12;
fn byte(reg: usize) -> u8 {
    unsafe { ((USB + reg) as *const u8).read_volatile() }
}
fn put(reg: usize, v: u8) {
    unsafe { ((USB + reg) as *mut u8).write_volatile(v) }
}
fn half(reg: usize) -> u16 {
    unsafe { ((USB + reg) as *const u16).read_volatile() }
}
fn put_half(reg: usize, v: u16) {
    unsafe { ((USB + reg) as *mut u16).write_volatile(v) }
}
fn phy(reg: usize, clear: u32, set: u32) {
    let p = (PHY + reg) as *mut u32;
    unsafe {
        p.write_volatile((p.read_volatile() & !clear) | set);
        core::arch::asm!("dsb sy");
    }
}
const DEVICE: [u8; 18] = [18, 1, 0, 2, 0, 0, 0, 64, 9, 0x12, 1, 0, 0, 1, 0, 1, 0, 1];
const CONFIG: [u8; 18] = [9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 0xff, 0, 0, 0];
fn snapshot() -> [u8; 32] {
    let radio = 0x09200000 as *const u32;
    let power = 0x09201000 as *const u32;
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(b"VDB1");
    let words = unsafe {
        [
            ticks() / ticks_per_ms,
            power.read_volatile(),
            radio.read_volatile(),
            radio.add(1).read_volatile(),
            radio.add(2).read_volatile(),
            radio.add(3).read_volatile(),
            radio.add(4).read_volatile(),
        ]
    };
    for (dest, word) in b[4..].chunks_exact_mut(4).zip(words) {
        dest.copy_from_slice(&word.to_le_bytes())
    }
    b
}
fn radio_snapshot() -> [u8; 52] {
    let radio = 0x09200000 as *const u32;
    let power = 0x09201000 as *const u32;
    let mut b = [0; 52];
    b[..4].copy_from_slice(b"VRD2");
    let words = unsafe {
        [
            radio.add(5).read_volatile(),
            radio.add(6).read_volatile(),
            radio.add(7).read_volatile(),
            power.add(1).read_volatile(),
            radio.add(8).read_volatile(),
            radio.add(9).read_volatile(),
            radio.add(10).read_volatile(),
            radio.add(11).read_volatile(),
            radio.add(12).read_volatile(),
            radio.add(13).read_volatile(),
            radio.add(14).read_volatile(),
            radio.add(15).read_volatile(),
        ]
    };
    for (out, word) in b[4..].chunks_exact_mut(4).zip(words) {
        out.copy_from_slice(&word.to_le_bytes());
    }
    b
}
fn radio_registers() -> [u8; 64] {
    let mut out = [0; 64];
    out[..4].copy_from_slice(b"VRG1");
    for (i, word) in out[4..].chunks_exact_mut(4).enumerate() {
        let sample = unsafe { (0x09200000 as *const u32).add(28 + i).read_volatile() };
        word.copy_from_slice(&sample.to_le_bytes());
    }
    out
}
fn scan_name() -> [u8; 37] {
    let mut out = [0; 37];
    out[..4].copy_from_slice(b"VSN1");
    let n = unsafe { (0x09200040 as *const u32).read_volatile() }.min(32) as usize;
    out[4] = n as u8;
    for i in 0..n {
        out[5 + i] = unsafe { ((0x09200044 + i) as *const u8).read_volatile() };
    }
    out
}
fn runtime_snapshot() -> [u8; 60] {
    let p = 0x09202000 as *const u32;
    let mut b = [0; 60];
    // 15 words: generation, focus, last fault, ready, disk, 40 bytes of status.
    for (i, chunk) in b.chunks_exact_mut(4).enumerate() {
        chunk.copy_from_slice(&unsafe { p.add(i).read_volatile() }.to_le_bytes());
    }
    b
}
fn send(bytes: &[u8], max: u16) {
    // EP0 uses one FIFO for both directions. Release the SETUP packet
    // and wait for transmit mode before filling it with the IN reply.
    put_half(CSR, 0x40);
    let start = unsafe { ticks() };
    while half(CSR) & 1 != 0 {
        if unsafe { ticks().wrapping_sub(start) >= ticks_per_ms * 10 } {
            put_half(CSR, 0x20);
            return;
        }
    }
    let data = &bytes[..bytes.len().min(usize::from(max))];
    let mut chunks = data.chunks_exact(4);
    for chunk in chunks.by_ref() {
        let word = u32::from_le_bytes(chunk.try_into().unwrap());
        unsafe { ((USB + 0x20) as *mut u32).write_volatile(word) };
    }
    for &v in chunks.remainder() {
        put(0x20, v);
    }
    put_half(CSR, 2 | 8);
}
fn upload_request(key:u32, offset:u32, data:&[u8]) -> bool {
    let mut m=Message::new(EXECUTE,key,offset);
    if key==0x60 {
        m.arg=u32::from_le_bytes(data[..4].try_into().unwrap());
    } else {
        m.arg=data.len() as u32;
        m.put_text(data);
    }
    m.call(7).op==OK
}
fn package_status() -> [u8;24] {
    let r=Message::new(EXECUTE,0x62,0).call(7);
    let mut out=[0;24];
    out[..4].copy_from_slice(b"VPKS");
    out[4..8].copy_from_slice(&r.key.to_le_bytes());
    out[8..12].copy_from_slice(&r.value.to_le_bytes());
    out[12..16].copy_from_slice(&r.arg.to_le_bytes());
    out[16..24].copy_from_slice(&r.data[..8]);
    out
}
fn progress(stage:u32,link:u32) {
    let mut msg=Message::new(EXECUTE,0x63,stage);msg.arg=link;msg.call(7);
}
pub fn run() -> ! {
    progress(1,0);
    let supply=Message::new(EXECUTE,0x4057,0).call(6);
    progress(if supply.op==OK {3}else{2},0);
    put(1, 0);
    unsafe { delay_ms(250) };
    // MT6765 usb_phy_recover followed by PHY_DEV_ACTIVE. Fastboot can
    // leave forced session-end asserted when handing off the controller.
    // The MT6765 device tree selects generic-tphy-v1. Pulse SUSPENDM
    // before releasing software overrides, as its power-on routine does.
    phy(0x00,0,1<<5);
    phy(0x14,1<<11,0);
    phy(0x68,(3<<30)|(1<<3),1<<18);
    phy(0x68,0,1<<3);
    unsafe { delay_ms(1) };
    phy(0x68,(1<<18)|(1<<3),0);
    phy(0x1c, 1 << 12, 0);
    phy(
        0x68,
        (1 << 26) | (1 << 23) | (0x1f << 17) | (0xf << 10) | 0xf4,
        0,
    );
    phy(0x6c, 1 << 16, 0);
    phy(0x20, 0x300, 0);
    phy(0x18, (1 << 23) | (0xff << 24), (1 << 20) | (0x40 << 24));
    unsafe { delay_ms(1) };
    // Set B-device role, session valid and VBUS valid before SOFTCONN.
    phy(0x6c, 0x10, 0x2f | (0x3f << 8));
    put(0x0e, 0);
    put(0, 0);
    put_half(6, 1);
    put(0x0b, 4);
    put(0x60, byte(0x60) | 1);
    put(1, 0x40);
    progress(4,u32::from(byte(0x60)));
    let mut pending_out: Option<(u32,u32,usize)>=None;
    // A multi-packet install write: window offset, bytes expected, bytes so far.
    let mut install_out: Option<(usize,usize,usize)>=None;
    let mut busy = 0u32;
    let mut address = None;
    let mut configuration = 0;
    let mut action_after_status = None;
    loop {
        let bus = byte(0x0a);
        if bus != 0 {
            put(0x0a, bus);
        }
        if bus & 4 != 0 {
            progress(5,u32::from(byte(0x60)));
            pending_out=None;
            install_out=None;
            address = None;
            configuration = 0;
            action_after_status = None;
            put(0, 0);
            put(0x0e, 0);
        }
        let tx = half(2) & 1;
        if tx != 0 {
            put_half(2, tx);
        }
        let csr = half(CSR);
        if csr & 0x10 != 0 {
            put_half(CSR, 0x80);
            action_after_status = None;
            address = None;
            pending_out=None;
            install_out=None;
            continue;
        }
        if csr & 4 != 0 {
            put_half(CSR, 0);
            continue;
        }
        if tx != 0 {
            if let Some((action, token)) = action_after_status.take() {
                if action == 0x70 {
                    // Power commits the MT6357 RTC fast-boot flag, then the
                    // supervisor flushes storage and resets. A failed reset
                    // returns here, so clear the flag for later reboots.
                    if Message::new(EXECUTE, 0x4062, 1).call(6).op == OK {
                        Message::new(EXECUTE, 0x65, 0).call(7);
                        Message::new(EXECUTE, 0x4062, 0).call(6);
                    }
                } else {
                    Message::new(EXECUTE, action, token).call(7);
                }
            }
            if let Some(a) = address.take() {
                put(0, a);
            }
        }
        if csr & 1 == 0 {
            // Stay awake briefly after traffic so back-to-back packets and
            // requests are not each held up by a millisecond's sleep.
            if busy > 0 {
                busy -= 1;
                core::hint::spin_loop();
            } else {
                unsafe { delay_ms(1) };
            }
            continue;
        }
        busy = 200_000;
        if let Some((at,total,got))=install_out {
            // Install data in 64-byte packets, straight into the window.
            let n=usize::from(half(0x18));
            if n==0 || got+n>total {
                install_out=None;
                put_half(CSR,0x60);
                continue;
            }
            for i in 0..n {
                let b=byte(0x20);
                unsafe { ((0x09211000 + at + got + i) as *mut u8).write_volatile(b) };
            }
            if got+n==total {
                install_out=None;
                put_half(CSR,0x48);
            } else {
                install_out=Some((at,total,got+n));
                put_half(CSR,0x40);
            }
            continue;
        }
        if let Some((key,offset,n))=pending_out.take() {
            if usize::from(half(0x18)) != n {
                put_half(CSR,0x60);
                continue;
            }
            let mut bytes=[0;40];
            for v in &mut bytes[..n] {*v=byte(0x20);}
            // 0x4c: a program for the Open Runtime to start, as a path.
            let ok=if key==0x4c {
                open_request(&bytes[..n])
            } else if key==0x4a {
                // Install data: bytes for the window at `offset`.
                let at = offset as usize;
                if at + n <= 65536 {
                    for (i, b) in bytes[..n].iter().enumerate() {
                        unsafe { ((0x09211000 + at + i) as *mut u8).write_volatile(*b) };
                    }
                    true
                } else { false }
            } else if key==0x49 {
                // Install commit: file offset, length, name; then bump the serial.
                let control = 0x09210000 as *mut u32;
                unsafe {
                    for (i, b) in bytes[..n].iter().enumerate() {
                        ((0x09210000 + 16 + i) as *mut u8).write_volatile(*b);
                    }
                    for i in n..40 {
                        ((0x09210000 + 16 + i) as *mut u8).write_volatile(0);
                    }
                    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
                    control.write_volatile(control.read_volatile().wrapping_add(1));
                }
                true
            } else {
                upload_request(key,offset,&bytes[..n])
            };
            put_half(CSR,if ok {0x48} else {0x60});
            continue;
        }
        if half(0x18) != 8 {
            put_half(CSR, 0x40 | 0x20);
            continue;
        }
        let mut s = [0u8; 8];
        for b in &mut s {
            *b = byte(0x20)
        }
        let val = u16::from_le_bytes([s[2], s[3]]);
        let index = u16::from_le_bytes([s[4], s[5]]);
        let len = u16::from_le_bytes([s[6], s[7]]);
        match (s[0], s[1], val, index) {
            (0x80, 6, 0x100, 0) => send(&DEVICE, len),
            (0x80, 6, 0x200, 0) => send(&CONFIG, len),
            (0x80, 6, 0x300, 0) => send(&[4, 3, 9, 4], len),
            (0x80, 6, 0x301, 0x409 | 0) => {
                let mut desc = [0u8; 42];
                let name = b"VioletOS Diagnostics";
                desc[0] = 42;
                desc[1] = 3;
                for (p, c) in desc[2..].chunks_exact_mut(2).zip(name) {
                    p[0] = *c;
                }
                send(&desc, len)
            }
            (0, 5, a, 0) if a < 128 && len == 0 => {
                address = Some(a as u8);
                put_half(CSR, 0x48);
            }
            (0, 9, c, 0) if c <= 1 && len == 0 => {
                configuration = c as u8;
                progress(7,u32::from(byte(0x60)));
                put_half(CSR, 0x48);
            }
            (0x80, 8, 0, 0) => send(&[configuration], len),
            (0x80 | 0x81 | 0x82, 0, 0, 0) => send(&[0, 0], len),
            (0xc0, 0x64, 0, 0) if configuration == 1 && len == 8 => {
                let reply = Message::new(EXECUTE, 0x64, 0).call(7);
                if reply.op == OK {
                    let mut packet = *b"VSW1....";
                    packet[4..].copy_from_slice(&reply.value.to_le_bytes());
                    send(&packet, len);
                } else {
                    put_half(CSR, 0x60);
                }
            }
            (0xc0, 0x6d, 0, 0) if configuration == 1 && (len == 12 || len == 16) => {
                let r = Message::new(EXECUTE, 0x6d, 0).call(7);
                let mut health = *b"VUD1............";
                health[4..8].copy_from_slice(&r.value.to_le_bytes());
                health[8..12].copy_from_slice(&r.arg.to_le_bytes());
                health[12..16].copy_from_slice(&r.data[..4]);
                if r.op == OK { send(&health, len); } else { put_half(CSR, 0x60); }
            }
            (0x40, 0x6e, lo, hi) if configuration == 1 && len == 0 => {
                action_after_status = Some((0x6e, u32::from(lo) | (u32::from(hi) << 16)));
                put_half(CSR, 0x48);
            }
            (0x40,0x6c,lo,hi) if configuration==1 && len==0 => {
                let token=u32::from(lo)|(u32::from(hi)<<16);
                let r=Message::new(EXECUTE,0x6c,token).call(7);
                put_half(CSR,if r.op==OK {0x48} else {0x60});
            }
            (0xc0,0x58,0,0) if configuration==1 => {
                let mut result=[0;44];
                for (i,chunk) in result.chunks_exact_mut(4).enumerate() {
                    chunk.copy_from_slice(&unsafe {((0x09200100 as *const u32).add(i)).read_volatile()}.to_le_bytes());
                }
                send(&result,len);
            }
            (0x40, 0x69, 0, 0) if configuration == 1 && len == 0 => {
                let reply = Message::new(EXECUTE, 0x69, 0).call(7);
                put_half(CSR, if reply.op == OK { 0x48 } else { 0x60 });
            }
            (0x40, action @ (0x65 | 0x68 | 0x70), 0, 0) if configuration == 1 && len == 0 => {
                action_after_status = Some((u32::from(action), 0));
                put_half(CSR, 0x48);
            }
            (0xc0, 0x62, 0, 0) if configuration==1 => send(&package_status(),len),
            // An install's state: commit serial, done serial, result.
            (0xc0, 0x48, 0, 0) => {
                let mut out = [0; 12];
                for (i, dest) in out.chunks_exact_mut(4).enumerate() {
                    dest.copy_from_slice(&unsafe { (0x09210000 as *const u32).add(i).read_volatile() }.to_le_bytes());
                }
                send(&out, len);
            }
            // Install data up to 4 KiB per request (many fewer round trips).
            (0x40, 0x4a, lo, hi) if configuration==1 && (1..=4096).contains(&len)
                && (usize::from(lo)|(usize::from(hi)<<16)) + usize::from(len) <= 65536 => {
                install_out=Some((usize::from(lo)|(usize::from(hi)<<16),usize::from(len),0));
                put_half(CSR,0x40);
            }
            // 0x6f: join options for the supervisor (a station address, then a host name).
            (0x40, key @ (0x49|0x4a|0x4c|0x60|0x61|0x66|0x67|0x6a|0x6b|0x6f), lo, hi) if configuration==1
                && (if key==0x60 {len==4} else {(1..=40).contains(&len)}) => {
                pending_out=Some((u32::from(key),u32::from(lo)|(u32::from(hi)<<16),usize::from(len)));
                put_half(CSR,0x40);
            }
            (0xc0, 0x57, 0, 0) => {
                let src = 0x09202044 as *const u32;
                let mut out = [0; 40];
                for (i, dest) in out.chunks_exact_mut(4).enumerate() {
                    dest.copy_from_slice(&unsafe { src.add(i).read_volatile() }.to_le_bytes());
                }
                send(&out, len);
            }
            (0xc0, 0x56, 0, 0) => {
                let previous = unsafe { (0x09202040 as *const u32).read_volatile() };
                let mut out = *b"VBH1....";
                out[4..].copy_from_slice(&previous.to_le_bytes());
                send(&out, len)
            }
            (0xc0, 0x55, 0, 0) => send(&radio_registers(), len),
            (0xc0, 0x54, 0, 0) => send(&scan_name(), len),
            (0xc0, 0x53, 0, 0) => send(&radio_snapshot(), len),
            (0xc0, 0x52, 0, 0) => send(&runtime_snapshot(), len),
            (0xc0, 0x51, 0, 0) => send(&snapshot(), len),
            // Bluetooth telemetry from Radio (targets/moto-g-pure/bt.rs): 64 bytes per page.
            (0xc0, 0x74, 0, page) if page < 24 => {
                let mut out = [0; 64];
                let src = (0x09200800 + usize::from(page) * 64) as *const u32;
                for (i, dest) in out.chunks_exact_mut(4).enumerate() {
                    dest.copy_from_slice(&unsafe { src.add(i).read_volatile() }.to_le_bytes());
                }
                send(&out, len);
            }
            // Audio status page, 16 words per request selected by the index.
            (0xc0, 0x4f, 0, page) if page < 6 => {
                let mut out = [0; 64];
                let src = (0x09203000 + usize::from(page) * 64) as *const u32;
                for (i, dest) in out.chunks_exact_mut(4).enumerate() {
                    dest.copy_from_slice(&unsafe { src.add(i).read_volatile() }.to_le_bytes());
                }
                send(&out, len);
            }
            // The supervisor's record of an Open Runtime fault: tag, label, four registers.
            (0xc0, 0x4b, 0, 0) => {
                // Then the supervisor's setup stage, slots, loader verdict, image size
                // and the first reservation that overlapped the open-apps window.
                let mut out = [0; 56];
                for (i, dest) in out.chunks_exact_mut(4).enumerate() {
                    dest.copy_from_slice(&unsafe { (0x09202000 as *const u32).add(40 + i).read_volatile() }.to_le_bytes());
                }
                send(&out, len);
            }
            // The install window, 64 bytes per request: a screenshot strip the Open
            // Runtime wrote there ("@shot", see usb_shot in runtime/open/mod.rs).
            (0xc0, 0x47, 0, chunk) if chunk < 1024 => {
                let mut out = [0; 64];
                let src = (0x09211000 + usize::from(chunk) * 64) as *const u8;
                for (i, dest) in out.iter_mut().enumerate() {
                    *dest = unsafe { src.add(i).read_volatile() };
                }
                send(&out, len);
            }
            // The Open Runtime's log ring (4 KiB), 64 bytes per request.
            (0xc0, 0x4d, 0, chunk) if chunk < 64 => {
                let mut out = [0; 64];
                let src = (0x09205000 + usize::from(chunk) * 64) as *const u8;
                for (i, dest) in out.iter_mut().enumerate() {
                    *dest = unsafe { src.add(i).read_volatile() };
                }
                send(&out, len);
            }
            // Camera's status page, 16 words per request selected by the index.
            (0xc0, 0x43, 0, page) if page < 6 => {
                let mut out = [0; 64];
                let src = (0x09207000 + usize::from(page) * 64) as *const u32;
                for (i, dest) in out.chunks_exact_mut(4).enumerate() {
                    dest.copy_from_slice(&unsafe { src.add(i).read_volatile() }.to_le_bytes());
                }
                send(&out, len);
            }
            // Camera's 64 KiB preview, 64 bytes per request.
            (0xc0, 0x42, 0, chunk) if chunk < 1024 => {
                let mut out = [0; 64];
                let src = (0x09230000 + usize::from(chunk) * 64) as *const u8;
                for (i, dest) in out.iter_mut().enumerate() {
                    *dest = unsafe { src.add(i).read_volatile() };
                }
                send(&out, len);
            }
            // A developer camera request: command in the high byte.
            (0x40, 0x44, lo, hi) if configuration == 1 && len == 0 => {
                let request = 0x09208000 as *mut u32;
                unsafe {
                    request.add(1).write_volatile(u32::from(lo) | (u32::from(hi) << 16));
                    let serial = request.read_volatile().wrapping_add(1);
                    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
                    request.write_volatile(serial);
                }
                put_half(CSR, 0x48);
            }
            // A developer audio request for Audio to act on: command in the high byte.
            (0x40, 0x4e, lo, hi) if configuration == 1 && len == 0 => {
                let request = 0x09204000 as *mut u32;
                unsafe {
                    request.add(1).write_volatile(u32::from(lo) | (u32::from(hi) << 16));
                    let serial = request.read_volatile().wrapping_add(1);
                    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
                    request.write_volatile(serial);
                }
                put_half(CSR, 0x48);
            }
            _ => put_half(CSR, 0x40 | 0x20),
        }
    }
}

/// Hand the Open Runtime a path to launch: word 0 counts requests, word 1 is
/// the length, the path follows from byte 8.
fn open_request(path: &[u8]) -> bool {
    let page = 0x09206000 as *mut u32;
    unsafe {
        let bytes = (0x09206008) as *mut u8;
        for (i, b) in path.iter().enumerate() {
            bytes.add(i).write_volatile(*b);
        }
        page.add(1).write_volatile(path.len() as u32);
        let serial = page.read_volatile().wrapping_add(1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        page.write_volatile(serial);
    }
    true
}
