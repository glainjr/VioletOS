//! Polled Connac AXI transport. Only the dedicated, uncached Moto DMA pool
//! supplies descriptor and payload addresses. One command can be in flight.
use super::radio_io::{barrier, byte, expired, now, put, rd, wr};
use super::{passive_scan, softap, wlan_download};
use wlan_download::Transport;
const DMA: usize = 0x09c00000;
const PA: u32 = 0x42400000;
const PDMA: usize = 0x09011000;
const SCHED: usize = 0x09014000;
const OWN: usize = 0x09012000;
const SYNC: usize = 0x09013140;
const GLO: usize = PDMA + 0x208;
const RINGS: [usize; 4] = [PDMA + 0x330, PDMA + 0x3f0, PDMA + 0x400, PDMA + 0x410];
const SLOTS: usize = 16;
const BUF: usize = 4096;
fn await_bits(addr: usize, mask: u32, want: u32, ms: u32) -> bool {
    let start = now();
    loop {
        if rd(addr) & mask == want {
            return true;
        }
        if expired(start, ms) {
            return false;
        }
    }
}
fn desc(r: usize, i: usize) -> usize {
    DMA + r * 0x1000 + 16 * i
}
fn payload(r: usize, i: usize) -> usize {
    0x10000 + (r * SLOTS + i) * BUF
}
pub struct Axi {
    cursor: [usize; 4],
    rx: [u8; BUF],
    original: u32,
    pause: u32,
    sleep_ack: u32,
    threshold: u32,
    owned: bool,
    pub station: [u8; 6],
    peer: [u8; 6],
    basic_rate: u8,
    pairwise_ready: bool,
    /// Joined to an open network: data frames go and come unencrypted.
    open_link: bool,
    sequence: u8,
    /// Access-point role: configuration, and 0 idle / 1 authenticated / 2 associated client.
    pub ap: softap::Config,
    pub ap_mode: bool,
    pub client_state: u8,
    pub mgmt_frames: u32,
    pub new_client: bool,
    /// Hosting telemetry: every frame and every management frame seen on rings 2/3.
    pub rx_frames: u32,
    pub rx_mgmt: u32,
}
impl Axi {
    pub const fn new() -> Self {
        Self {
            cursor: [0; 4],
            rx: [0; BUF],
            original: 0,
            pause: 0,
            sleep_ack: 0,
            threshold: 0,
            owned: false,
            station: [0; 6],
            peer: [0; 6],
            basic_rate: 0,
            pairwise_ready: false,
            open_link: false,
            sequence: 0,
            ap: softap::Config::empty(),
            ap_mode: false,
            client_state: 0,
            mgmt_frames: 0,
            new_client: false,
            rx_frames: 0,
            rx_mgmt: 0,
        }
    }
    fn init(&mut self) -> Result<(), u32> {
        wr(OWN, 2);
        if !await_bits(OWN, 1, 0, 250) {
            return Err(31);
        }
        self.original = rd(GLO);
        super::radio_io::trace(4, self.original);
        // The Ellis reset value enables OMIT_TX_INFO (bit 28), as expected for Connac.
        if self.original & (15 | 0x180 | 0xa8000000) != 0 {
            return Err(32);
        }
        // A live or foreign ring must never be resumed with our DMA settings.
        for i in 0..16 {
            let count = rd(PDMA + 0x304 + i * 16) & 0xffff;
            if count != 0 {
                super::radio_io::trace(5, (i as u32) << 16 | count);
                return Err(32);
            }
        }
        for i in 0..2 {
            let count = rd(PDMA + 0x404 + i * 16) & 0xffff;
            if count != 0 {
                super::radio_io::trace(5, ((16 + i) as u32) << 16 | count);
                return Err(32);
            }
        }
        self.pause = rd(PDMA + 0x224);
        self.sleep_ack = rd(PDMA + 0x160);
        self.threshold = rd(PDMA + 0x260);
        self.owned = true;
        // The factory SoC1.0 setup assigns traffic to group zero. The ring 3
        // bypass is used only for ROM download; runtime commands need credits.
        let sizes = rd(SCHED + 0x1c);
        wr(SCHED + 0x1c, (sizes & !0x0fff0fff) | 0x00180001);
        wr(SCHED + 0x10, 0xfffe0000);
        wr(SCHED + 0x20, 0x0fff0003);
        for group in 1..15 {
            wr(SCHED + 0x20 + 4 * group, 0);
        }
        wr(SCHED + 0xd0, 0);

        wr(PDMA + 0x224, 0);
        wr(PDMA + 0x160, self.sleep_ack | 16);
        wr(PDMA + 0x260, 0x00020002);
        for (r, reg) in RINGS.into_iter().enumerate() {
            for i in 0..SLOTS {
                let d = desc(r, i);
                wr(d, PA + payload(r, i) as u32);
                wr(
                    d + 4,
                    if r >= 2 {
                        (BUF as u32) << 16
                    } else {
                        0x80000000
                    },
                );
                wr(d + 8, 0);
                wr(d + 12, 0);
            }
            wr(reg, PA + (r * 0x1000) as u32);
            wr(reg + 8, if r >= 2 { SLOTS as u32 - 1 } else { 0 });
            wr(reg + 4, SLOTS as u32);
            if rd(reg) != PA + (r * 0x1000) as u32
                || rd(reg + 4) & 0xffff != SLOTS as u32
                || rd(reg + 12) & 0xffff != 0
            {
                return Err(33);
            }
        }
        // MT6765 uses 16-byte descriptors and little-endian payloads.
        let mode = (self.original & !((3 << 10) | (1 << 26))) | 0x40001a75;
        wr(GLO, mode);
        if rd(GLO) & 5 != 5 {
            return Err(33);
        }
        Ok(())
    }
    fn send(&mut self, r: usize, frame: &[u8]) -> Result<(), u32> {
        if frame.len() > BUF {
            return Err(34);
        }
        let i = self.cursor[r];
        let d = desc(r, i);
        super::radio_io::trace(
            3,
            ((r as u32) << 24) | ((i as u32) << 16) | (frame.len() as u32),
        );
        super::radio_io::trace(4, rd(GLO));
        if rd(d + 4) >> 31 == 0 {
            return Err(34);
        }
        let off = payload(r, i);
        for (k, b) in frame.iter().enumerate() {
            put(DMA + off + k, *b);
        }
        wr(d + 4, 0x40000000 | ((frame.len() as u32) << 16));
        let next = (i + 1) % SLOTS;
        wr(RINGS[r] + 8, next as u32);
        self.cursor[r] = next;
        if !await_bits(d + 4, 0x80000000, 0x80000000, 500) {
            super::radio_io::trace(5, rd(RINGS[r] + 12));
            return Err(35);
        }
        Ok(())
    }
    fn take(&mut self, r: usize) -> Result<Option<usize>, u32> {
        let i = self.cursor[r];
        let d = desc(r, i);
        let flag = rd(d + 4);
        if flag >> 31 == 0 {
            return Ok(None);
        }
        barrier();
        let n = ((flag >> 16) & 0x3fff) as usize;
        if n == 0 || n > BUF || flag & 0x40000000 == 0 {
            return Err(36);
        }
        for (k, b) in self.rx[..n].iter_mut().enumerate() {
            *b = byte(DMA + payload(r, i) + k);
        }
        wr(d + 4, (BUF as u32) << 16);
        wr(RINGS[r] + 8, i as u32);
        self.cursor[r] = (i + 1) % SLOTS;
        Ok(Some(n))
    }
    pub fn shutdown(&mut self) -> bool {
        if !self.owned {
            return true;
        }
        wr(GLO, rd(GLO) & !5);
        if !await_bits(GLO, 10, 0, 100) {
            return false;
        }
        for reg in RINGS {
            wr(reg + 4, 0);
            wr(reg, 0);
        }
        wr(PDMA + 0x224, self.pause);
        wr(PDMA + 0x160, self.sleep_ack);
        wr(PDMA + 0x260, self.threshold);
        wr(GLO, self.original);
        self.owned = false;
        true
    }
    fn normal_command(&mut self, cid: u8, seq: u8, set: bool, payload: &[u8]) -> Result<(), u32> {
        let mut b = [0; 64 + passive_scan::SCAN_BYTES];
        let len = 64 + payload.len();
        if len > b.len() {
            return Err(34);
        }
        b[..2].copy_from_slice(&(len as u16).to_le_bytes());
        b[3] = 0x80;
        b[5] = 0xa0;
        b[7] = 2;
        b[32..34].copy_from_slice(&((len - 32) as u16).to_le_bytes());
        b[36..40].copy_from_slice(&[cid, 0xa0, set as u8, seq]);
        b[42] = 3;
        b[64..len].copy_from_slice(payload);
        self.send(1, &b[..len])
    }
    fn station_address(&mut self, seq: u8) -> Result<[u8; 6], u32> {
        // Motorola's wlanQueryNicCapability sends a 116-byte query body.
        self.normal_command(0x80, seq, false, &[0; 116])?;
        super::radio_io::trace(5, u32::from(seq) << 24);
        super::radio_io::trace(6, 0);
        super::radio_io::trace(7, 0);
        let started = now();
        while !expired(started, 1000) {
            let Some(n) = self.take(3)? else { continue };
            super::radio_io::trace(5, (u32::from(seq) << 24) | n as u32);
            if n >= 26 {
                super::radio_io::trace(6, u32::from_le_bytes(self.rx[16..20].try_into().unwrap()));
                super::radio_io::trace(7, u32::from_le_bytes(self.rx[20..24].try_into().unwrap()));
            }
            if n < 28 || self.rx[2..4] != [0, 0xe0] {
                continue;
            }
            let len = u16::from_le_bytes([self.rx[0], self.rx[1]]) as usize;
            if len < 28 || len > n {
                continue;
            }
            let event = &self.rx[16..len];
            if event[2..6] != [0, 0xe0, 1, seq] {
                continue;
            }
            let size = u16::from_le_bytes([event[0], event[1]]) as usize;
            if size < 12 + 116 || size > event.len() {
                return Err(45);
            }
            let mut mac: [u8; 6] = event[20..26].try_into().unwrap();
            if let Some(fixed) = super::radio_io::fixed_address() {
                return Ok(fixed);
            }
            if mac == [0; 6] || mac[0] & 1 != 0 {
                // Ellis can provide the permanent address outside the NIC.
                // Configure a locally administered unicast address for this session.
                if !super::radio_io::address_entropy(&mut mac) {
                    return Err(45);
                }
                mac[0] = (mac[0] & !3) | 2;
            }
            return Ok(mac);
        }
        Err(45)
    }
    fn channel(&mut self, channel: u8, token: u8, abort: bool) -> Result<(), u32> {
        self.channel_for(0, channel, token, abort, 0, 3000)
    }
    fn channel_for(
        &mut self,
        bss: u8,
        channel: u8,
        token: u8,
        abort: bool,
        kind: u8,
        interval: u32,
    ) -> Result<(), u32> {
        let band = match channel {
            1..=11 => 1,
            36 | 40 | 44 | 48 | 149 | 153 | 157 | 161 | 165 => 2,
            _ => return Err(46),
        };
        self.sequence = self.sequence.wrapping_add(1);
        let mut body = [0; 24];
        body[0] = bss;
        body[1] = token;
        body[2] = abort as u8;
        body[3] = channel;
        body[5] = band;
        body[9] = kind; // CH_REQ_TYPE_JOIN or CH_REQ_TYPE_GO_START_BSS
        body[10] = 4; // firmware selects radio band
        body[12..16].copy_from_slice(&interval.to_le_bytes());
        self.normal_command(0x1c, self.sequence, true, &body)?;
        if abort {
            return Ok(());
        }
        let start = now();
        while !expired(start, if kind == 0 { 1000 } else { 3000 }) {
            let Some(n) = self.take(3)? else { continue };
            let packet = &self.rx[..n];
            if n >= 32 {
                super::radio_io::trace(6, u32::from_le_bytes(packet[16..20].try_into().unwrap()));
                super::radio_io::trace(7, u32::from_le_bytes(packet[28..32].try_into().unwrap()));
            }
            if n < 44 || packet[2..4] != [0, 224] || packet[18..21] != [0, 224, 16] {
                continue;
            }
            let reported = u16::from_le_bytes([packet[0], packet[1]]) as usize;
            let event_len = u16::from_le_bytes([packet[16], packet[17]]) as usize;
            if reported > n || event_len < 28 || event_len + 16 > reported {
                continue;
            }
            let grant = &packet[28..];
            if grant[0] == bss
                && grant[1] == token
                && grant[2] == 0
                && grant[3] == channel
                && grant[5] == band
                && grant[9] == 0
            {
                // Two timed exchanges follow. Reject a grant too short for both.
                if kind == 0 && u32::from_le_bytes(grant[12..16].try_into().unwrap()) < 1600 {
                    return Err(46);
                }
                return Ok(());
            }
        }
        Err(46)
    }

    fn management_exchange(
        &mut self,
        ap: &passive_scan::AccessPoint,
        associate: bool,
    ) -> Result<u16, u32> {
        let mut frame = [0; 128];
        let n = super::station::request(ap, self.station, associate, &mut frame);
        let mut tx = [0; 160];
        tx[..2].copy_from_slice(&((32 + n) as u16).to_le_bytes());
        tx[2] = 12;
        tx[3] = 0x84; // MCU management queue 1
        tx[5] = 0xcc; // long descriptor, native 802.11, 24-byte MAC header
        tx[7] = 2;
        tx[8] = frame[0] >> 4;
        tx[11] = 0x80;
        tx[26] = super::station::basic_rate(ap);
        tx[12..14].copy_from_slice(&(7u16 << 11).to_le_bytes());
        tx[32..32 + n].copy_from_slice(&frame[..n]);
        self.send(1, &tx[..32 + n])?;
        let start = now();
        while !expired(start, 700) {
            for ring in [2, 3] {
                let Some(n) = self.take(ring)? else { continue };
                if let Some(frame) = passive_scan::management_frame(&self.rx[..n]) {
                    if let Some(result) =
                        super::station::response(frame, ap, self.station, associate)
                    {
                        return result;
                    }
                }
            }
        }
        Err(if associate { 48 } else { 47 })
    }

    fn set(&mut self, cid: u8, payload: &[u8]) -> Result<(), u32> {
        self.sequence = self.sequence.wrapping_add(1);
        self.normal_command(cid, self.sequence, true, payload)
    }
    fn peer_event(&mut self, event: u8, ap: [u8; 6]) -> Result<(), u32> {
        let start = now();
        while !expired(start, 700) {
            let Some(n) = self.take(3)? else { continue };
            let p = &self.rx[..n];
            if n < 36 || p[2..4] != [0, 224] || p[18..21] != [0, 224, event] {
                continue;
            }
            let length = u16::from_le_bytes([p[0], p[1]]) as usize;
            let event_len = u16::from_le_bytes([p[16], p[17]]) as usize;
            if length > n || event_len < 20 || event_len + 16 > length {
                continue;
            }
            if (event == 12 && p[28..34] == ap && p[34..36] == [0, 0])
                || (event == 0x24 && p[28] == 0 && p[30..36] == ap)
            {
                return Ok(());
            }
        }
        Err(51)
    }
    fn commit_peer(&mut self, ap: &passive_scan::AccessPoint, aid: u16) -> Result<u16, u32> {
        let (bss, sta) = super::station::firmware_peer(ap, aid);
        let mut filter = [0; 68];
        filter[0] = 0x09; // Directed and broadcast frames.
        self.set(0x0a, &filter)?;
        self.set(0x12, &bss)?;
        self.set(0x13, &sta)?;
        self.peer_event(12, ap.bssid)?;
        self.peer = ap.bssid;
        self.basic_rate = super::station::basic_rate(ap);
        Ok(aid)
    }
    pub fn transmit_eapol(&mut self, data: &[u8]) -> Result<(), u32> {
        if data.len() > 1536 {
            return Err(52);
        }
        let mut ethernet = [0; 1550];
        ethernet[..6].copy_from_slice(&self.peer);
        ethernet[6..12].copy_from_slice(if self.ap_mode { &self.ap.bssid } else { &self.station });
        ethernet[12..14].copy_from_slice(&[0x88, 0x8e]);
        ethernet[14..14 + data.len()].copy_from_slice(data);
        self.transmit_ethernet(&ethernet[..14 + data.len()], self.pairwise_ready)
    }
    pub fn send_ip(&mut self, ethernet: &[u8]) -> Result<(), u32> {
        if self.ap_mode {
            return self.ap_send_ip(ethernet);
        }
        if !self.pairwise_ready
            || ethernet.len() < 14
            || ethernet.len() > 1518
            || ethernet[6..12] != self.station
            || !matches!(&ethernet[12..14], [8, 0] | [8, 6])
        {
            return Err(54);
        }
        self.transmit_ethernet(ethernet, !self.open_link)
    }
    /// The station link can carry data: keys installed, or an open network.
    pub fn link_keyed(&self) -> bool {
        self.pairwise_ready
    }
    fn transmit_ethernet(&mut self, ethernet: &[u8], protected: bool) -> Result<(), u32> {
        let mut p = [0; 1582];
        let n = 32 + ethernet.len();
        p[..2].copy_from_slice(&(n as u16).to_le_bytes());
        // Long descriptor, Ethernet II header translation, pairwise WTBL entry.
        p[2..6].copy_from_slice(&[8, 0x84, 1, 0x90]);
        p[7] = 2 | if self.ap_mode { softap::OWN_MAC << 2 } else { 0 };
        if !protected {
            p[11] = 0x80;
            p[26] = self.basic_rate;
        }
        p[12..14].copy_from_slice(&((7u16 << 11) | if protected { 2 } else { 0 }).to_le_bytes());
        p[32..n].copy_from_slice(ethernet);
        self.send(1, &p[..n])
    }
    pub fn poll_data(&mut self, out: &mut [u8], eapol: &mut [u8]) -> Result<(usize, usize), u32> {
        if self.ap_mode {
            return self.ap_poll(out, eapol);
        }
        for ring in [2, 3] {
            let Some(n) = self.take(ring)? else { continue };
            let p = &self.rx[..n];
            if let Some(frame) = super::station::eapol(p, &self.station, &self.peer) {
                if frame.len() <= eapol.len() {
                    eapol[..frame.len()].copy_from_slice(frame);
                    return Ok((0, frame.len()));
                }
            }
            if let Some(n) = super::station::ethernet(p, &self.station, &self.peer, out, self.open_link) {
                return Ok((n, 0));
            }
            if let Some(f) = passive_scan::management_frame(p) {
                if f.len() >= 26
                    && matches!(f[0], 0xa0 | 0xc0)
                    && f[10..16] == self.peer
                    && f[16..22] == self.peer
                    && (f[4..10] == self.station || f[4..10] == [255; 6])
                {
                    return Err(54);
                }
            }
        }
        Ok((0, 0))
    }
    pub fn receive_eapol(&mut self, out: &mut [u8]) -> Result<usize, u32> {
        for ring in [2, 3] {
            let Some(n) = self.take(ring)? else { continue };
            if n >= 16 {
                let p = &self.rx[..n];
                if p[3] >> 5 == 2 || (p[3] >> 5 == 7 && p[2] & 15 == 1) {
                    super::radio_io::trace(5, u32::from_le_bytes(p[..4].try_into().unwrap()));
                    super::radio_io::trace(6, u32::from_le_bytes(p[8..12].try_into().unwrap()));
                    if let Some(f) = passive_scan::rx_payload(p) {
                        let ether = if p[6] & 128 != 0 { f.get(12..14) } else { None };
                        super::radio_io::trace(7, u32::from(p[6]) << 16 | ether.map_or(0, |v| u32::from(u16::from_be_bytes([v[0],v[1]]))));
                    }
                }
            }
            if let Some(data) = super::station::eapol(&self.rx[..n], &self.station, &self.peer) {
                if data.len() <= out.len() {
                    out[..data.len()].copy_from_slice(data);
                    return Ok(data.len());
                }
            }
        }
        Ok(0)
    }
    pub fn install_ccmp(
        &mut self,
        pair: bool,
        id: u8,
        key: &[u8],
        counter: &[u8],
    ) -> Result<(), u32> {
        if key.len() != 16 || counter.len() > 8 || id > 3 {
            return Err(52);
        }
        let mut body = [0; 64];
        body[..3].copy_from_slice(&[1, pair as u8, pair as u8]);
        body[4..10].copy_from_slice(&self.peer);
        if self.ap_mode {
            body[3] = 1; // authenticator
            body[10] = softap::BSS;
        }
        body[11..15].copy_from_slice(&[4, id, 16, if pair { 1 } else { 2 }]);
        body[16..32].copy_from_slice(key);
        body[48..48 + counter.len()].copy_from_slice(counter);
        self.set(7, &body)?;
        if pair {
            if self.ap_mode {
                // As with the station record, the key event layout differs for a client
                // of our BSS; data flow after the handshake is the real confirmation.
                let confirmed = self.peer_event(0x24, self.peer).is_ok();
                super::radio_io::trace(4, 0x4b45_0000 | confirmed as u32);
            } else {
                self.peer_event(0x24, self.peer)?;
            }
            self.pairwise_ready = true;
        }
        Ok(())
    }


    /// Start beaconing as an access point. Mirrors gen4m p2pFuncStartGO:
    /// channel grant, activate the P2P-network BSS, BSS info, beacon, PM BSS created.
    pub fn start_ap(&mut self, ssid: &[u8], channel: u8, wpa2: bool) -> Result<(), u32> {
        if ssid.is_empty() || ssid.len() > 32 {
            return Err(56);
        }
        let mut bssid = self.station;
        bssid[0] = (bssid[0] | 2) & !1;
        bssid[5] ^= 1;
        self.ap = softap::Config { ssid: [0; 32], ssid_len: ssid.len(), bssid, channel, wpa2 };
        self.ap.ssid[..ssid.len()].copy_from_slice(ssid);
        self.ap_mode = true;
        self.client_state = 0;
        self.pairwise_ready = false;
        // gen4m activates the role BSS when the interface is created, before any channel request.
        let mut activate = [softap::BSS, 1, 1, softap::OWN_MAC, 0, 0, 0, 0, 0, 0, softap::GROUP_WTBL, 0];
        activate[4..10].copy_from_slice(&bssid);
        self.set(0x11, &activate).map_err(|_| 57u32)?;
        // The grant is advisory here: with no station BSS active the firmware may
        // simply adopt the BSS channel from the RLM parameters below.
        let token = self.sequence.wrapping_add(1);
        let granted = self.channel_for(softap::BSS, channel, token, false, 3, 5000).is_ok();
        super::radio_io::trace(4, 0x4150_0000 | granted as u32);
        let mut filter = [0; 68];
        filter[0] = 0x09;
        self.set(0x0a, &filter)?;
        let info = self.ap.bss_info();
        self.set(0x12, &info).map_err(|_| 57u32)?;
        let mut beacon = [0; 256];
        let n = self.ap.beacon_template(&mut beacon);
        self.set(0x18, &beacon[..n]).map_err(|_| 58u32)?;
        let created = self.ap.pm_created();
        self.set(0x15, &created).map_err(|_| 58u32)?;
        Ok(())
    }
    pub fn peer_address(&self) -> [u8; 6] {
        self.peer
    }
    fn ap_mgmt(&mut self, frame: &[u8], wlan: u8) -> Result<(), u32> {
        let n = frame.len();
        let mut tx = [0; 32 + 256];
        tx[..2].copy_from_slice(&((32 + n) as u16).to_le_bytes());
        tx[2] = 12;
        tx[3] = 0x84; // MCU management queue 1
        tx[4] = wlan;
        tx[5] = 0xcc; // long descriptor, native 802.11, 24-byte MAC header
        tx[7] = 2 | (softap::OWN_MAC << 2);
        tx[8] = frame[0] >> 4;
        tx[11] = 0x80;
        tx[12..14].copy_from_slice(&(7u16 << 11).to_le_bytes());
        tx[26] = 0; // 1 Mbit/s CCK
        tx[32..32 + n].copy_from_slice(frame);
        self.send(1, &tx[..32 + n])
    }
    fn ap_manage(&mut self, request: softap::Request) -> Result<(), u32> {
        let mut out = [0; 256];
        match request {
            softap::Request::Probe(to) => {
                let n = self.ap.probe_response(&to, &mut out);
                self.ap_mgmt(&out[..n], 0)?;
            }
            softap::Request::Auth(from) => {
                // One phone at a time; a new authentication replaces the previous client.
                self.peer = from;
                self.client_state = 1;
                self.pairwise_ready = false;
                let record = softap::client_record(&from, 0, 0, 0, false);
                self.set(0x13, &record)?;
                let n = self.ap.auth_response(&from, &mut out);
                self.ap_mgmt(&out[..n], softap::CLIENT_WTBL)?;
            }
            softap::Request::Assoc { from, reassoc, listen, rsn } => {
                if from != self.peer || self.client_state == 0 || rsn != self.ap.wpa2 {
                    return Ok(());
                }
                let n = self.ap.assoc_response(&from, reassoc, 1, &mut out);
                self.ap_mgmt(&out[..n], softap::CLIENT_WTBL)?;
                let record = softap::client_record(&from, 1, listen, 2, true);
                self.set(0x13, &record)?;
                // The station-record event confirms activation for an AP's own peer;
                // for a client of our BSS its layout differs, so treat it as advisory.
                let confirmed = self.peer_event(12, from).is_ok();
                super::radio_io::trace(4, 0x4153_0000 | confirmed as u32);
                self.client_state = 2;
                self.new_client = true;
                self.basic_rate = 0;
                // Open networks carry data immediately; WPA2 waits for the 4-way handshake.
                self.pairwise_ready = !self.ap.wpa2;
            }
            softap::Request::Leave(from) => {
                if from == self.peer {
                    self.client_state = 0;
                    self.pairwise_ready = false;
                }
            }
        }
        Ok(())
    }
    fn ap_send_ip(&mut self, ethernet: &[u8]) -> Result<(), u32> {
        if self.client_state != 2 || !self.pairwise_ready {
            return Ok(()); // no phone associated: drop, as a real link would
        }
        if ethernet.len() < 14
            || ethernet.len() > 1518
            || ethernet[6..12] != self.ap.bssid
            || !matches!(&ethernet[12..14], [8, 0] | [8, 6])
        {
            return Err(54);
        }
        // Only one phone is associated: deliver broadcasts to it as unicast so the
        // pairwise key and per-station rate table apply.
        let mut frame = [0; 1518];
        frame[..ethernet.len()].copy_from_slice(ethernet);
        if frame[0] & 1 != 0 {
            frame[..6].copy_from_slice(&self.peer);
        }
        if frame[..6] != self.peer {
            return Ok(());
        }
        self.transmit_ethernet(&frame[..ethernet.len()], self.ap.wpa2)
    }
    /// Service management frames and return the next data frame (Ethernet) or EAPOL body.
    pub fn ap_poll(&mut self, out: &mut [u8], eapol: &mut [u8]) -> Result<(usize, usize), u32> {
        for ring in [2, 3] {
            let Some(n) = self.take(ring)? else { continue };
            self.rx_frames = self.rx_frames.wrapping_add(1);
            super::radio_io::trace(6, self.rx_frames | (ring as u32) << 28);
            if let Some(f) = passive_scan::management_frame(&self.rx[..n]) {
                let kind = f[0];
                self.rx_mgmt = self.rx_mgmt.wrapping_add(1);
                super::radio_io::trace(7, u32::from(kind) << 24 | u32::from(f[4]) << 16 | u32::from(f[9]) << 8 | u32::from(f[15]));
                if let Some(request) = softap::classify(f, &self.ap) {
                    self.mgmt_frames = self.mgmt_frames.wrapping_add(1);
                    super::radio_io::trace(5, u32::from(kind) << 24 | self.mgmt_frames);
                    self.ap_manage(request)?;
                }
                continue;
            }
            let p = &self.rx[..n];
            if self.client_state != 2 {
                continue;
            }
            if let Some(frame) = super::station::eapol(p, &self.ap.bssid, &self.peer) {
                if frame.len() <= eapol.len() {
                    eapol[..frame.len()].copy_from_slice(frame);
                    return Ok((0, frame.len()));
                }
            }
            if let Some(len) = softap::ethernet(p, &self.ap, &self.peer, out) {
                if out[12..14] == [0x88, 0x8e] {
                    // Trim link-layer padding: the MIC covers exactly the EAPOL frame.
                    let body = &out[14..len];
                    let declared = body.get(2..4).map_or(0, |b| 4 + u16::from_be_bytes([b[0], b[1]]) as usize);
                    let size = declared.min(body.len()).min(eapol.len());
                    eapol[..size].copy_from_slice(&body[..size]);
                    return Ok((0, size));
                }
                return Ok((len, 0));
            }
        }
        Ok((0, 0))
    }
    pub fn associate(&mut self, ap: &passive_scan::AccessPoint) -> Result<u16, u32> {
        if !(ap.wpa2_psk_ccmp || ap.open) || ap.rate_count == 0 {
            return Err(49);
        }
        // A rejoin starts unkeyed. The keys of the last link are gone on the access
        // point's side, so message 2 of the new handshake must go out in the clear
        // (transmit_eapol protects it only once a pairwise key is installed).
        self.pairwise_ready = false;
        self.open_link = false;
        let token = self.sequence.wrapping_add(1);
        let result = self
            .channel(ap.channel, token, false)
            .and_then(|()| self.management_exchange(ap, false))
            .and_then(|_| self.management_exchange(ap, true))
            .and_then(|aid| self.commit_peer(ap, aid));
        self.channel(ap.channel, token, true)?;
        if result.is_ok() && ap.open {
            // No handshake on an open network: association alone admits us.
            self.open_link = true;
            self.pairwise_ready = true;
        }
        result
    }
    /// Upload and start the WLAN firmware, then read the station address.
    /// Returns the last command sequence number used.
    pub fn boot(&mut self, image: &[u8], stage: fn(u32)) -> Result<u8, u32> {
        stage(31);
        self.init()?;
        let mut seq = 0;
        stage(37);
        wlan_download::upload(image, &mut seq, self).map_err(|e| match e {
            wlan_download::Error::Bus(s) => s,
            _ => 37u32,
        })?;
        stage(38);
        let mut start = [0; 72];
        start[..8].copy_from_slice(&[72, 0, 0, 128, 0, 160, 0, 2]);
        start[32..40].copy_from_slice(&[40, 0, 0, 0, 2, 160, 0, seq.wrapping_add(1)]);
        self.command(&start)?;
        let ack = self.command_result()?;
        if ack.len() < 12 || ack[4] != 1 || ack[5] != start[39] || ack[8] != 0 {
            return Err(38);
        }
        if !await_bits(SYNC, 6, 6, 1500) {
            return Err(39);
        }
        wr(GLO, rd(GLO) & !(1 << 9));
        stage(40);
        seq = seq.wrapping_add(2);
        self.station = self.station_address(seq)?;
        self.sequence = seq;
        Ok(seq)
    }
    pub fn scan(
        &mut self,
        image: &[u8],
        result: &mut passive_scan::Results,
        stage: fn(u32),
    ) -> Result<(), u32> {
        let mut seq = self.boot(image, stage)?;
        let mut activate = [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0];
        activate[4..10].copy_from_slice(&self.station);
        seq = seq.wrapping_add(1);
        self.normal_command(0x11, seq, true, &activate)?;
        let scan_id = seq.wrapping_add(1);
        self.sequence = scan_id;
        self.normal_command(3, scan_id, true, &passive_scan::request(scan_id))?;
        let start = now();
        while !expired(start, 4500) {
            for r in [2, 3] {
                if let Some(n) = self.take(r)? {
                    if result.receive(&self.rx[..n], scan_id) {
                        return Ok(());
                    }
                }
            }
        }
        Err(41)
    }
}
impl Transport for Axi {
    type Error = u32;
    fn command(&mut self, frame: &[u8]) -> Result<(), u32> {
        self.send(0, frame)
    }
    fn data(&mut self, frame: &[u8]) -> Result<(), u32> {
        self.send(0, frame)
    }
    fn command_result(&mut self) -> Result<&[u8], u32> {
        let start = now();
        while !expired(start, 1000) {
            if let Some(n) = self.take(3)? {
                if n < 28 {
                    return Err(36);
                }
                super::radio_io::trace(6, u32::from_le_bytes(self.rx[20..24].try_into().unwrap()));
                super::radio_io::trace(7, u32::from_le_bytes(self.rx[24..28].try_into().unwrap()));
                return Ok(&self.rx[16..n]);
            }
        }
        Err(42)
    }
}
