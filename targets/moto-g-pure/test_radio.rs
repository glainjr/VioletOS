//! Host tests against the exact bundle that the handset loader embeds.
mod conn_firmware;
mod passive_scan;
mod softap;
mod station;
mod wlan_download;
mod wlan_image;
const FACTORY: &[u8] = include_bytes!(env!("VIOLET_TEST_WIFI_BUNDLE"));

fn entry(i: usize) -> (usize, usize) {
    let p = 8 + i * 8;
    let word = |start| u32::from_le_bytes(FACTORY[start..start + 4].try_into().unwrap()) as usize;
    (word(p), word(p + 4))
}

#[test]
fn rom_payloads_and_build_dates() {
    let mut ram = vec![0xa5; 4 << 20];
    let mut expected = 619824;
    for (i, offset, date) in [
        (0, 0, 0x68320),
        (1, 0x140000, 0x68310),
        (6, 0x80000, 0x68300),
    ] {
        let (p, n) = entry(i);
        expected += n - 48;
        if i == 0 {
            assert!(conn_firmware::stage(FACTORY, &mut ram).is_some());
        }
        assert_eq!(&ram[offset..offset + n - 48], &FACTORY[p + 48..p + n]);
        assert_eq!(ram[offset + n - 48], 0xa5);
        assert_eq!(&ram[date..date + 15], &FACTORY[p..p + 15]);
        assert_eq!(ram[date + 15], 0);
    }
    assert_eq!(conn_firmware::stage(FACTORY, &mut ram), Some(expected));
}

#[test]
fn malformed_rom_and_bundle_leave_memory_untouched() {
    let mut ram = vec![0xa5; 4 << 20];
    for index in [0, 8, entry(6).0 + 24, entry(0).0 + 16, entry(1).0 + 31] {
        let mut bad = FACTORY.to_vec();
        bad[index] ^= 1;
        assert_eq!(conn_firmware::stage(&bad, &mut ram), None);
    }
    assert_eq!(conn_firmware::stage(&FACTORY[..64], &mut ram), None);
    assert!(ram.iter().all(|&b| b == 0xa5));
}

#[test]
fn wlan_descriptor_cannot_overwrite_connectivity_boot_rom() {
    let (p, len) = entry(3);
    let mut bad = FACTORY.to_vec();
    let dest = p + len - 36 - 5 * 40 + 3 * 40 + 16;
    bad[dest..dest + 4].copy_from_slice(&0xf0000000u32.to_le_bytes());
    let mut ram = vec![0xa5; 4 << 20];
    assert_eq!(conn_firmware::stage(&bad, &mut ram), None);
    assert!(ram.iter().all(|&b| b == 0xa5));
}

#[derive(Default)]
struct RecordedDownload {
    commands: Vec<Vec<u8>>,
    data: Vec<Vec<u8>>,
    event: [u8; 12],
}
impl wlan_download::Transport for RecordedDownload {
    type Error = ();
    fn command(&mut self, frame: &[u8]) -> Result<(), ()> {
        self.event[5] = frame[39];
        self.commands.push(frame.to_vec());
        Ok(())
    }
    fn command_result(&mut self) -> Result<&[u8], ()> {
        Ok(&self.event)
    }
    fn data(&mut self, frame: &[u8]) -> Result<(), ()> {
        self.data.push(frame.to_vec());
        Ok(())
    }
}

#[test]
fn controller_download_preserves_ciphertext_and_limits_frame_sizes() {
    let (p, n) = entry(3);
    let image = &FACTORY[p..p + n];
    let mut bus = RecordedDownload::default();
    bus.event[..5].copy_from_slice(&[12, 0, 0, 0xe0, 1]);
    let mut seq = 254;
    wlan_download::upload(image, &mut seq, &mut bus).unwrap();
    assert_eq!(bus.commands.len(), 3);
    assert_eq!(bus.data.len(), 8);
    assert_eq!(seq, 9);
    let offsets = [0x57000u32, 0x02010000, 0x40100200];
    let sizes = [4016usize, 336, 9616];
    for ((frame, dest), size) in bus.commands.iter().zip(offsets).zip(sizes) {
        assert_eq!(frame[..8], [76, 0, 0, 128, 0, 160, 0, 2]);
        assert_eq!(frame[32..39], [44, 0, 0, 0, 1, 160, 0]);
        assert_eq!(frame[64..68], dest.to_le_bytes());
        assert_eq!(frame[68..72], (size as u32).to_le_bytes());
        assert_eq!(frame[72..76], [15, 0, 0, 128]);
    }
    let mut copied = Vec::new();
    for frame in bus.data {
        assert!(frame.len() <= 2112);
        assert_eq!(
            frame.len(),
            u16::from_le_bytes([frame[0], frame[1]]) as usize
        );
        assert_eq!(frame[2..8], [0, 248, 0, 160, 0, 3]);
        copied.extend_from_slice(&frame[64..]);
    }
    assert_eq!(copied, image[..sizes.iter().sum()]);
}
#[test]
fn rejected_configuration_sends_no_firmware() {
    let (p, n) = entry(3);
    let mut bus = RecordedDownload::default();
    let mut seq = 0;
    assert_eq!(
        wlan_download::upload(&FACTORY[p..p + n], &mut seq, &mut bus),
        Err(wlan_download::Error::BadAck)
    );
    bus.event[..5].copy_from_slice(&[12, 0, 0, 0xe0, 1]);
    bus.event[8] = 2;
    assert_eq!(
        wlan_download::upload(&FACTORY[p..p + n], &mut seq, &mut bus),
        Err(wlan_download::Error::Rejected(2))
    );
    assert!(bus.data.is_empty());
}

#[test]
fn passive_scan_request_and_received_beacon() {
    let wire = passive_scan::request(71);
    assert_eq!(wire.len(), 0x4a4);
    assert_eq!((wire[0], wire[2], wire[5], wire[159]), (71, 0, 0, 20));
    for (i, ch) in wire[160..182].chunks_exact(2).enumerate() {
        assert_eq!(ch, [1, i as u8 + 1]);
    }
    assert_eq!(
        &wire[182..200],
        &[2, 36, 2, 40, 2, 44, 2, 48, 2, 149, 2, 153, 2, 157, 2, 161, 2, 165]
    );
    let mut results = passive_scan::Results::default();
    let mut beacon = [0u8; 61];
    beacon[..4].copy_from_slice(&[61, 0, 1, 224]);
    beacon[16] = 0x80;
    beacon[52..].copy_from_slice(b"\0\x07fixture");
    assert!(!results.receive(&beacon, 71));
    assert_eq!(&results.name[..results.name_len], b"fixture");
    let mut done = [0u8; 36];
    done[..4].copy_from_slice(&[36, 0, 0, 224]);
    done[16..20].copy_from_slice(&[20, 0, 0, 224]);
    done[20] = 13;
    done[28] = 71;
    done[32] = 20;
    done[33] = 7;
    assert!(!results.receive(&done, 72));
    assert!(results.receive(&done, 71));
    assert_eq!(results.frames, 1);
}

#[test]
fn identifies_wpa2_personal_access_point_on_channel_161() {
    let mut frame = vec![0; 36];
    frame[0] = 0x80;
    frame[16..22].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
    frame[34] = 0x11;
    frame.extend_from_slice(b"\x00\x04test");
    frame.extend_from_slice(&[
        61, 1, 161, 48, 18, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 2,
    ]);
    let mut r = passive_scan::Results::default();
    r.frame(&frame);
    r.frame(&frame);
    assert_eq!(r.ap_count, 1);
    assert_eq!(r.aps[0].channel, 161);
    assert!(r.aps[0].wpa2_psk_ccmp);
    let mut required = frame.clone();
    required[46] = 20;
    required.extend_from_slice(&[0x40, 0]); // management frame protection required
    r.frame(&required);
    assert!(!r.aps[0].wpa2_psk_ccmp);
    *frame.last_mut().unwrap() = 1; // enterprise AKM
    r.frame(&frame);
    assert!(!r.aps[0].wpa2_psk_ccmp);
}

#[path = "test_radio_bus.rs"]
mod radio_io;
mod wlan_axi;
mod wmt;

#[test]
fn register_level_scan_and_dma_timeout_paths() {
    for fault in 0..6 {
        radio_io::reset();
        radio_io::wr(0x09011208, 0x10001870);
        radio_io::with(|m| {
            m.no_dma_completion = fault == 1;
            m.drop_scan_done = fault == 2;
            m.no_ack = fault == 3;
            m.wrong_seq = fault == 4;
            m.invalid_mac = fault == 5;
        });
        let mut axi = wlan_axi::Axi::new();
        let mut results = passive_scan::Results::default();
        let (p, n) = entry(3);
        let rc = axi.scan(&FACTORY[p..p + n], &mut results, |_| {});
        assert_eq!(
            rc,
            [Ok(()), Err(35), Err(41), Err(42), Err(37), Ok(())][fault]
        );
        if fault == 0 {
            assert_eq!(&results.name[..results.name_len], b"test-net");
            assert_eq!(results.channels, 20);
            radio_io::with(|m| assert_eq!(m.ciphertext, FACTORY[p..p + 13968]));
        }
        assert!(axi.shutdown());
        assert_eq!(radio_io::rd(0x09011208) & 5, 0);
    }
}

#[test]
fn provisioned_address_replaces_the_chip_and_random_ones() {
    for (invalid_mac, fixed_mac, expected) in [
        (false, None, [2, 17, 34, 51, 68, 85]),
        (true, None, [0xfe, 1, 2, 3, 4, 5]),
        (false, Some([2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]), [2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]),
        (true, Some([2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]), [2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]),
    ] {
        radio_io::reset();
        radio_io::wr(0x09011208, 0x10001870);
        radio_io::with(|m| {
            m.invalid_mac = invalid_mac;
            m.fixed_mac = fixed_mac;
        });
        let mut axi = wlan_axi::Axi::new();
        let mut results = passive_scan::Results::default();
        let (p, n) = entry(3);
        axi.scan(&FACTORY[p..p + n], &mut results, |_| {}).unwrap();
        assert_eq!(axi.station, expected);
        assert!(axi.shutdown());
    }
}

#[test]
fn associates_only_on_selected_channel_with_confirmed_aid() {
    for (wrong_channel, denied, expected) in [
        (false, false, Ok(3)),
        (true, false, Err(46)),
        (false, true, Err(49)),
    ] {
        radio_io::reset();
        radio_io::with(|m| {
            m.wrong_channel = wrong_channel;
            m.deny_association = denied;
        });
        let mut axi = wlan_axi::Axi::new();
        let mut scan = passive_scan::Results::default();
        let (p, n) = entry(3);
        axi.scan(&FACTORY[p..p + n], &mut scan, |_| {}).unwrap();
        let mut ap = passive_scan::AccessPoint::default();
        ap.bssid = [2, 5, 6, 7, 8, 9];
        ap.channel = 161;
        ap.wpa2_psk_ccmp = true;
        ap.ssid_len = 4;
        ap.ssid[..4].copy_from_slice(b"test");
        ap.rates[..3].copy_from_slice(&[0x8c, 0x98, 0xb0]);
        ap.rate_count = 3;
        assert_eq!(axi.associate(&ap), expected);
        radio_io::with(|m| {
            let last = m.commands.last().unwrap();
            assert_eq!(last[36], 0x1c);
            assert_eq!(last[66], 1); // Release the channel on every outcome.
        });
        if expected.is_ok() {
            let mut ethernet = [0u8; 64];
            ethernet[..6].copy_from_slice(&[255; 6]);
            ethernet[6..12].copy_from_slice(&axi.station);
            ethernet[12..14].copy_from_slice(&[8, 6]);
            assert!(axi.send_ip(&ethernet).is_err());
            axi.transmit_eapol(&[2, 3, 0, 0]).unwrap();
            radio_io::with(|m| {
                let tx = m.commands.last().unwrap();
                assert_eq!(tx[12] & 2, 0);
                assert_eq!(&tx[44..46], &[0x88, 0x8e]);
                m.drop_key_done = true;
            });
            assert_eq!(axi.install_ccmp(true, 0, &[42; 16], &[]), Err(51));
            assert!(axi.send_ip(&ethernet).is_err());
            radio_io::with(|m| m.drop_key_done = false);
            axi.install_ccmp(true, 0, &[42; 16], &[]).unwrap();
            axi.install_ccmp(false, 1, &[24; 16], &[1, 0, 0, 0, 0, 0])
                .unwrap();
            axi.send_ip(&ethernet).unwrap();
            radio_io::with(|m| {
                let tx = m.commands.last().unwrap();
                assert_eq!(tx[12] & 2, 2);
                assert_eq!(&tx[32..], &ethernet);
            });
        }
        assert!(axi.shutdown());
    }
}

fn reply_frame(body: &[u8], full_seq: Option<u8>) -> Vec<u8> {
    let mut frame = vec![128, 64 | ((body.len() >> 8) as u8), body.len() as u8, 0];
    let mut crc = 0u16;
    if let Some(s) = full_seq {
        frame[0] |= (s & 7) << 3;
        frame[3] = frame[0].wrapping_add(frame[1]).wrapping_add(frame[2]);
        for &b in body {
            crc ^= u16::from(b);
            for _ in 0..8 {
                let low = crc & 1;
                crc >>= 1;
                if low != 0 {
                    crc ^= 0xa001;
                }
            }
        }
    }
    frame.extend_from_slice(body);
    frame.extend_from_slice(&crc.to_le_bytes());
    frame
}
#[test]
fn wmt_programmed_io_and_radio_profiles() {
    let (p, n) = entry(2);
    let patch = &FACTORY[p..p + n];
    for profile in [1, 2] {
        radio_io::reset();
        let query = [2, 4, 6, 0, 0, 4, 17, 0, 0, 0];
        let configured = [2, 4, 6, 0, 0, 4, 219, 14, 104, 1];
        let mut events = Vec::new();
        for event in [
            &query[..],
            &[2, 1, 1, 0, 0],
            &[2, 7, 1, 0, 0],
            &query,
            &[2, 4, 2, 0, 0, 3],
        ] {
            events.extend(reply_frame(event, None));
        }
        let mut after = vec![&configured[..], &[2, 2, 1, 0, 0], &[2, 16, 1, 0, 0]];
        if profile == 2 {
            after.push(&[2, 16, 1, 0, 0]);
        }
        let mut calibration = [0u8; 454];
        calibration[..8].copy_from_slice(&[2, 20, 0xc2, 1, 0, 1, 1, 0x15]);
        after.extend([&calibration[..], &[2, 6, 1, 0, 0]]);
        for (i, event) in after.into_iter().enumerate() {
            events.extend(reply_frame(event, Some(i as u8)));
        }
        radio_io::with(|m| m.bt_rx.extend(events));
        assert_eq!(wmt::enable(patch, profile, |_| {}), Ok(()));
        radio_io::with(|m| {
            assert!(m.bt_rx.is_empty());
            assert!(m.bt_tx.windows(9).any(|w| w == [1, 2, 5, 0, 0x11, 0x65, 0x67, 0, 0]));
            assert!(m.bt_tx.windows(n - 30).any(|w| w == &patch[30..]));
        });
    }
    radio_io::reset();
    assert_eq!(wmt::enable(patch, 0, |_| {}), Err(16));
    assert_eq!(wmt::enable(patch, 1, |_| {}), Err(22));
}

#[test]
fn authorized_ip_requires_hardware_ccmp_and_accepts_translated_frames() {
    let mac = [2, 0, 0, 0, 0, 1];
    let ap = [2, 0, 0, 0, 0, 2];
    let mut p = [0; 64];
    p[..4].copy_from_slice(&[64, 0, 0, 0x40]);
    p[6] = 0x80;
    p[8] = 1;
    p[9] = 0x40;
    p[16..22].copy_from_slice(&mac);
    p[22..28].copy_from_slice(&ap);
    p[28..30].copy_from_slice(&[8, 0]);
    let mut out = [0; 64];
    assert_eq!(station::ethernet(&p, &mac, &ap, &mut out, false), Some(48));
    assert_eq!(&out[..48], &p[16..]);
    for (offset, wrong) in [(9, 0), (10, 16), (8, 7), (16, 4), (0, 70)] {
        let mut bad = p;
        bad[offset] = wrong;
        assert!(station::ethernet(&bad, &mac, &ap, &mut out, false).is_none());
    }
    p[28..30].copy_from_slice(&[0x88, 0x8e]);
    p[30..36].copy_from_slice(&[2, 3, 0, 2, 1, 2]);
    assert_eq!(station::eapol(&p, &mac, &ap), Some(&p[30..36]));
    p[2] = 1;
    p[3] = 0xe0;
    p[13] = 0x10;
    assert_eq!(station::eapol(&p, &mac, &ap), Some(&p[30..36]));
    p[22] ^= 1;
    assert!(station::eapol(&p, &mac, &ap).is_none());
}
