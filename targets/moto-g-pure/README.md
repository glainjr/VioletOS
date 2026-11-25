# Moto G Pure

The XT2163DL target boots seL4 on the physical handset and drives its display and touchscreen. The tested Rust build opened, started, paused and reset the stopwatch on slot B. Run `make moto` using the [factory inputs documented in the main README](../../README.md#handset-build-and-validation) to produce a local boot image.

The handset runs the shared supervisor, Runtime, launcher, stopwatch, display and catalog manager. The touch process owns the MT6765 SPI3, pin, clock and watchdog registers. The earlier RAM-only image passed the physical Start → Home → Stopwatch check.

## Handset storage

The storage image includes the same checkpoint and private-file implementation as QEMU. The [Moto driver](emmc.rs) uses MSDC0 to read or write one 512-byte eMMC sector and waits for the card to finish programming. If the inherited card cache is enabled, Flush also issues the eMMC cache-flush command. The [mount check](storage_volume.rs) verifies both GPT checksums, the exact factory boot B bounds (sectors 2,000,896 through 2,066,431), the Android boot header, and a VioletOS volume marker.

The image builder puts the marker 16 MiB into boot B and initializes the following 1 MiB for VioletOS. Files and checkpoints survive a normal reboot; reinstalling this complete image initializes the volume again. A failed mount leaves applications in RAM-only mode with a diagnostic on Home when available.

For hardware acceptance, check for **DISK READY**, then start and pause Stopwatch. Record the displayed time, reboot B without reflashing, and reopen Stopwatch to compare the restored time.

## Handset power and Wi-Fi bring-up

The Power package owns the MT6765 PMIC wrapper. It checks the MT6357 chip identity, requests a battery ADC conversion and reports millivolts. Screen blanking keeps touch running. The confirmed Off action requests shutdown by releasing the PMIC power-hold bit. Verify the voltage, tap-to-wake and shutdown on the handset. The QEMU power test exercises the virtual power backend.

## Round one on slot B

The Moto radio remains idle at Home. **Scan Wi-Fi** starts the first hardware access by the radio service and the passive scan. Home remains usable while Radio works. The test listens on 2.4 GHz channels 1–11. A completed scan displays the first visible network name, a hidden-network result or an empty result. The UUID HTTPS test requires association and an IP connection.

The loader selects the Ellis antenna profile from the bootloader’s device and radio identifiers. It checks the RAM layout and uses the factory secure-monitor interface for the 4 MiB connectivity window and separate 3 MiB DMA pool. Unknown profiles or conflicting memory protections stop the scan. The Radio process reads the factory firmware bundle from a separate read-only mapping. The connectivity processor must return its ROM-start signature before the driver sends the MCU patch over BTIF or submits WLAN commands over AXI.

The driver checks each firmware configuration acknowledgement, waits for WLAN readiness and sends the firmware a passive scan request with no probe requests. A completed scan requires a matching scan sequence, firmware completion state and all eleven channels. The network name comes from a received beacon or probe response. After sampling, the driver stops DMA and requests domain and regulator cleanup.

## Reading the result

With USB connected, run `make moto-debug` on the Mac. The read-only reader reports firmware bytes staged, regulator acknowledgements, chip ID, ROM mailbox, scan stage and result, received frame count, completed channels, first SSID and cleanup status. Home shows the result as well. The launcher’s private `radio-probe` file holds the last returned status.

| Diagnostic | Meaning |
| --- | --- |
| 2–10 | Initial power state, power controller, regulator, clock, chip identity or bus protection |
| 12–16 | ROM version, EMI remap, ROM-start timeout, memory permissions or antenna profile |
| Stage 20, 26–30 | BTIF query, patch transfer, framing switch, antenna configuration, calibration or WLAN power-on |
| Errors 21–25 | BTIF transmit or receive timeout, invalid frame, unexpected reply or patch header |
| Errors 31–36 | Controller ownership, idle state, DMA ring setup, transmit or receive descriptor |
| Stage 37–40 | WLAN firmware upload, firmware start, WLAN readiness and passive scan |
| Errors 37–42 | Upload rejection, start rejection, readiness or scan timeout, or missing firmware reply |
| Result 43 | Firmware completed the requested scan |
| Cleanup 44 | DMA did not acknowledge idle. The driver retains power and the reserved memory. |

Run `make test-moto-radio` for the factory-firmware tests, memory-protection tests and register-level driver fixtures, including timeout and rejection paths. `make test-moto-ui` checks the handset application catalog in QEMU with virtual peripherals. Physical radio startup, scan results and USB enumeration remain handset measurements.

The full slot B image initializes the 1 MiB VioletOS volume inside it. Flashing this image clears saved VioletOS files and stopwatch times. The previously tested battery image is the recovery image for this round.

During handset startup the loader draws a white band before relocating seL4. An amber square on the band means the relocated kernel copy completed. Green blocks above the band mark supervisor progress, from left to right: initial framebuffer mapping, core processes prepared, service workers wired, and Launcher started. The first normal screen replaces these marks. The loader reloads the inherited watchdog during bounded memory copies and leaves its reset policy in place.

`make test-moto-ui` boots the Moto loader, supervisor and catalog with QEMU’s interrupt controller and synthetic board handoff. It checks the four supervisor stages in guest memory. A separate run checks interactive stopwatch behavior with QEMU peripherals. Physical USB enumeration and reception of Wi-Fi beacons require handset evidence.

The November 20 handset trials did not produce a USB Wi-Fi result. The first image (`2d15ea03f183`) reached a visible white band and fell back to Android A. The second (`34f8a2b6b01b`) showed a white band and a green streak, followed by apparent restarts. The observed marks did not identify a specific supervisor stage. Neither trial establishes that the Wi-Fi chip received a scan command.


Three dedicated sectors after the VioletOS volume hold an image marker and alternating boot records. The loader checks the live GPT and both volume markers before writing. Each stage must flush and read back successfully. On the next VioletOS boot, **LAST** and a digit show the last committed stage. USB reports the same value as `previous_boot_stage`. Zero means a fresh history. 4294967295 means the loader could not read or update it. Android on A does not need to mount the VioletOS volume.

| Last stage | What finished |
| --- | --- |
| 1 | Loader opened the validated history area |
| 2 | Firmware handoff and memory-policy check finished |
| 3 | Loader prepared the jump to seL4 |
| 4 | seL4 ran the supervisor, which mapped the eMMC controller |
| 5 | Supervisor finished wiring the processes |
| 6 | Storage process opened the volume |

The first five stages precede the storage process taking ownership of eMMC. A reset before the first durable update cannot record a new stage. The record identifies the last completed write and has no reset-cause information.

On the XT2163DL, slot B image `6b1784307ba4` reached Home with the Stopwatch button visible and Wi-Fi left idle. This build uses a 720-pixel pitch for the boot diagnostics, a dedicated 32 KiB storage stack, and startup that allows Home to draw while touch and storage initialize. The Mac USB diagnostic reader did not receive a reply during the Home check. Wi-Fi scanning was not exercised in this trial.

Slot B installer image `07a0a5c68ed0` includes USB package transfer, two persistent catalog banks, and the Wi-Fi memory-policy diagnostic. Fastboot confirmed the 19,459 KiB write to `boot_b` and slot B selection on the XT2163DL. The embedded Radio payload checksum is `d0b9b77e`; the 24,608-byte R2 update has payload checksum `300e82e3`. Those are the expected values for the Mac installer status before and after updating. The full-image flash alone establishes neither update activation nor checkpoint persistence. See the [package transfer procedure](../../system/package-manager/README.md#moto-developer-packages).

On installer build `07a0a5c68ed0`, the handset displayed `USB 5 LINK 99`. The Mac detected the connection and repeatedly rejected the initial device descriptor because it read 111 for endpoint zero's maximum packet size. The compiled descriptor specifies 64. Build `fd5dd9d066ca` acknowledges the incoming setup packet, waits for the receive-ready bit to clear, and then writes the reply into the shared FIFO. This ordering follows Motorola's [endpoint-zero driver](https://github.com/MotorolaMobilityLLC/kernel-mtk/blob/MMI-S3RHS32.20-42-10-4-11/drivers/misc/mediatek/usb20/musb_gadget_ep0.c). The Moto Home and stopwatch tests passed under QEMU. USB enumeration requires a physical handset test.

The XT2163DL enumerated as **VioletOS Diagnostics** at 12 Mbps on macOS with build `fd5dd9d066ca`. Over USB, `moto-package status` reported factory Radio `031a79f6`, bank 0, and an empty inbox. `moto-debug` reported generation 1, touch ready, disk ready and zero recorded faults. The R2 package prepared for the live update has Radio payload checksum `300e82e3`.

With Stopwatch paused at 2.7 seconds, the Mac sent Radio R2 over the running VioletOS USB connection. The handset reported package ID 7 and 24,608 of 24,608 bytes received. Before on-device install confirmation, the active checksum remained `031a79f6` in factory bank 0.

After the handset install confirmation, the Mac read active Radio checksum `300e82e3` in catalog bank 2 and an empty inbox. The running supervisor reported generation 2, zero recorded faults, and disk ready. This confirms live activation on the handset without another boot-image flash.

After a physical restart through the startup menu, fastboot reported slot B selected. A plain reboot started that same image. At 8.5 seconds into the new boot, USB reported Radio checksum `300e82e3`, catalog bank 2, generation 1, previous boot stage 6, and disk ready. This confirms that the USB-installed Radio persisted across a handset reboot.

After reopening Stopwatch on that boot, the tester read 2.7 seconds, exactly matching the paused value before the USB transfer. The active Radio checksum was verified over USB and the restored time was verified on the handset screen.

**Hands-free Radio update on the XT2163DL.** After one bootstrap flash of developer image `d396cf7bb732`, `tools/moto-reboot-check --install-radio build/sel4/moto/radio-r2.vpk` completed from the Mac. The fresh test volume contained a deliberately seeded 4.1-second checkpoint. USB reported factory Radio `7e81a913` in bank 0, then R2 `300e82e3` in bank 2 after signed activation. The Mac requested a physical reboot. VioletOS returned at generation 1 with previous boot stage 6, R2 in bank 2 and the same 4.1-second checkpoint. Activation, reboot and both checkpoint comparisons ran without handset input.

Signed Radio R3 (`17d7889e`) exposed the loader's Wi-Fi prerequisite values over USB while idle on the XT2163DL. The Mac read RF profile 2 and policy `0x10`: the region 25 readback failed the loader's empty-or-expected mapping check. Firmware bytes staged, channels completed and received frames were all zero. The Wi-Fi hardware had not associated and the UUID request was not issued. Diagnostic boot image `eddc23577c73` also captures the four raw secure-monitor replies for each of regions 25 and 29. This allows the Mac to distinguish unexpected bounds, unexpected permissions and possible firmware error returns.

On the handset, diagnostic image `eddc23577c73` reached VioletOS after reselecting slot B following an initial fallback to Android A. All eight secure-monitor reads (start, end and two permission words for each of regions 25 and 29) returned `0xffffffff`. The original `0x10` classification therefore did not establish an occupied memory range. No Wi-Fi firmware was staged and no scan ran. The source now distinguishes an all-ones read failure from a conflicting region and still stops before configuring either window.

### Runtime Wi-Fi memory interface

In the new Moto baseline, the loader stages the firmware bundle and identifies the antenna profile. Radio requests a fresh memory inspection when it starts, including after a signed Radio-only update. Before a scan it requests preparation through the same supervisor endpoint. Both requests use key `RADIO_MEMORY` (`0x4058`). `GET` reads the windows and `EXECUTE` may prepare empty ones. The value selects automatic (0), primary (1), or offset (2) readback. The supervisor checks the caller's Radio badge and returns the policy status in the result value.

The Ellis AArch32 kernel extension gives the supervisor one board-specific capability in the reserved SMC slot. Its two operations are limited to the connectivity and DMA windows. The offset reader also checks the AP reference window. The supervisor checks the antenna profile and power state before preparation. General secure-monitor calls are unavailable through this interface. This local kernel extension has been exercised in tests and has no formal verification claim.

Use `build/moto-package scan-wifi` from the Mac while Home is visible. `build/moto-debug` reports completed runtime memory checks, the raw window replies and scan results. The QEMU firmware fixture returns unsupported reads. `python3 targets/moto-g-pure/test_boot.py --radio-package build/sel4/moto/radio-r4.vpk` exercises a separately built replacement Radio's startup IPC on the same QEMU kernel, without simulating a successful Wi-Fi scan.

On the physical XT2163DL, slot B baseline `3037d4f95c73` completed one runtime memory inspection at startup. Signed Radio R4 (`239e269d`) activated over USB in catalog bank 2, advanced the supervisor generation to 2, and completed a second inspection without a handset reboot. Sending `scan-wifi` from the Mac triggered the third call (preparation). Radio returned probe status 15 and memory policy `0x42`: primary window reads returned all ones and the alternate reader failed its AP reference-range check. USB reported zero firmware bytes staged, zero channels completed, and no process faults. No network association or UUID request occurred.

### WPA2-Personal and the HTTPS test

The Moto Radio package controls Wi-Fi startup, loads the vendor firmware, scans, authenticates with the selected access point, and runs the WPA2 four-way handshake. It requests the fixed power, memory and entropy operations from the supervisor. Radio owns the IPv4, DHCP, DNS, TCP and TLS clients. The Wi-Fi app displays the UUID returned by the HTTPS endpoint. The current station configuration supports WPA2-Personal with CCMP on the listed non-DFS channels and requires a network that permits association without mandatory protected management frames.

The ABI 3 Moto Radio package uses 128 KiB of private working memory supplied by the supervisor. The matching baseline supports signed Radio-only USB updates. With Home visible, run `tools/moto-wifi-profile` in a private terminal to provision the network for this boot, then run `build/moto-package join-wifi` to scan, join, and fetch the UUID. The provisioner transfers the derived key and Mac time over the developer USB cable. Runtime grants the Wi-Fi app access to the connection and test operations. The supervisor releases the key only to Radio. Use a trusted USB host with this developer image.

`make test-moto-radio` checks firmware command and frame handling and exercises WPA2 with a fixture access point. `make test-moto-ip-live` runs the handset's IP and TLS client on the host with libslirp providing the test Ethernet link.

On the physical XT2163DL, slot B image `4be2ac63c17e` with signed Radio `ca18deb9` completed a passive scan on all 20 configured channels. USB reported three received beacons or probe responses and zero process faults. The run used a Radio-only USB update. Signed Radio `c90eb9e1` then joined the provisioned WPA2 network, obtained a DHCP lease and fetched two distinct UUIDv4 responses over verified HTTPS on the same handset connection. The Mac used USB to request each test and read the result. The profile is held in memory for the current boot.

## Hands-free image flashing

`build/moto-package reboot-bootloader` asks the running VioletOS to restart into fastboot. USB Diagnostics acknowledges the request and asks Power to set the MT6357 RTC fast-boot flag, `RTC_PDN1` bit 13, the bit Motorola's `rtc_mark_fast()` sets. Power unlocks the RTC write interface, commits the bit through `RTC_WRTGR`, waits for `RTC_BBPU` to clear its busy bit, and reads the bit back. The supervisor then flushes storage and issues the usual TOPRGU software reset. If the reset does not happen, Diagnostics clears the flag so a later reboot still starts VioletOS. Register addresses come from Motorola's `include/linux/mfd/mt6357/registers.h` and `drivers/misc/mediatek/rtc/mt6357/`.

`services/usb-diagnostics/host/moto-flash [IMAGE]` writes an image to `boot_b`, selects slot B, reboots and waits for VioletOS to answer over USB. When run from VioletOS it sends `reboot-bootloader`, and from Android on A it runs `adb reboot bootloader`. If the running image cannot request fastboot, it reboots VioletOS repeatedly until slot B runs out of retries and the bootloader falls back to Android A. `--fastboot-only` stops in the bootloader without flashing.

**No more Android (November 25, 2026).** `moto-flash --take-userdata` erases Android on the handset: it writes VioletOS's claim (`VioletOS XT2163DL userdata v1`) to the first sector of Android's 23 GiB `userdata` partition and flashes the image to `boot_a` as well as `boot_b`. Storage finds `userdata` in the CRC-checked GPT and uses it only when that claim is there; on the first start it copies the installed apps from the end of `boot_b` (about two minutes, once) and records the move in the second sector. The installed apps then have about 23 GiB instead of 12 MiB; `/data` stays in `boot_b`. Slot A now holds VioletOS too, so a bad image in B falls back to a working VioletOS that answers over USB (`--slot-a` refreshes it). `super` (Android's 5 GiB system), the bootloaders and `nvdata` are untouched; Android comes back only by flashing Motorola's full firmware. Unit ZY22HWZ4M5 was converted: 9 installs moved, 24.7 GB free, VioletOS booted from slot A when selected, and a new install survived a restart.

The boot image carries the 1 MiB storage volume (app files, the Wi-Fi app's saved networks, checkpoints) as zeros. `moto-flash` therefore sends it as an Android sparse image whose storage window is a "don't care" chunk, so the bootloader writes everything except that window and the handset keeps its data across flashes; `--wipe-storage` sends the plain image instead. If the bootloader refuses the sparse image, `moto-flash` says so and falls back to the plain one. On November 24, 2026 the XT2163DL's bootloader accepted the sparse image, and `/data/networks` survived the flash.

## Wi-Fi reliability

[docs/wifi-reliability-rfc.md](../../docs/wifi-reliability-rfc.md) lists what keeps the station link up. In short: a rejoin after a link failure starts with no pairwise key, so the new handshake's message 2 goes out in the clear (it used to go out encrypted with the old key, and the access point dropped every rejoin); status polls no longer end a link that is being rejoined, and the event loop keeps rejoining (500 ms apart three times, then 2 s doubling up to a minute) for as long as the network stays joined; a single failed transmit no longer ends the link (32 in a row do); the supplicant's timers keep running inside every blocking network wait; a scan keeps 32 access points instead of 8; and open networks join without a handshake.

On November 21, 2026 the XT2163DL was running an image without the request. `moto-flash` exhausted slot B's retries in six reboots (about 300 s), reached Android A, entered fastboot with adb and flashed image `300e374ef5bb`. On that first boot, USB Diagnostics answered when the handset counter read 221 s. Later boots of the same image answered within 8 s. The cause of the first-boot delay was not determined. From that image, `reboot-bootloader` reached fastboot in 5 s with B current, bootable, and six retries remaining. A full `moto-flash` run from VioletOS reached fastboot at 11.1 s, finished the write and slot selection at 11.9 s, and VioletOS answered at 20.1 s with generation 1, disk ready, and a freshly initialized volume (previous boot stage 0). A following plain `moto-package reboot` returned to VioletOS in 11 s, confirming that the bootloader clears the flag. None of these runs needed handset input.

Each VioletOS boot still uses one slot B retry because VioletOS does not mark the slot successful. `moto-flash` restores the count with `set_active b`.

## USB diagnostics

The current build ships [USB Diagnostics](../../services/usb-diagnostics/README.md) as its own signed-update target. Its service guide covers the Mac clients, live package replacement and recovery tests.

## Phone relay access point

`moto-wifi-profile --save` keeps the network name and derived WPA2 key (not the password) in `~/.config/violetos/wifi-profile.json`, readable only by you; `moto-flash` provisions it after every flash, and `moto-wifi-profile --saved` does so after any other reboot. DHCP may take up to 30 s.

By default the phone is a private client: it joins with a random Wi-Fi address for each radio session and gives the network no name, so a router lists it as a new, unnamed device each time. Two environment variables for `moto-wifi-profile` change that for the current boot, once `VIOLET_IS_PRIVATE_CLIENT` is set to `0`, `no`, `false`, `off` or `disable`: `VIOLET_FIXED_MAC` (for example `02:11:22:33:44:55`) is the address the phone uses instead, and `VIOLET_HOSTNAME` is the name it sends with its DHCP requests. `VIOLET_SSID` and `VIOLET_WIFIPASS` answer the script's two questions the same way. `--save` keeps both settings and the choice with the profile.

A fixed address and a name let every network the phone joins know it again, which RFC 7844 (anonymity profiles for DHCP clients, §3.7 for the name) advises a private client against. That is why they need the second variable: without it the script says that it is not using them, and clears any set earlier in the boot. `VIOLET_IS_PRIVATE_CLIENT` set to `1`, `yes`, `true`, `on` or `enable` does the same over a choice kept with the profile. `make test-moto-wifi-profile` checks what the script sends and keeps, without a phone.

Radio also serves open apps' TCP sockets from its event loop (see the [Open Runtime](../../runtime/open/README.md#how-it-works)). A USB join now tries every access point broadcasting the network name, 2.4 GHz first, and a join of an ordinary network while hosting the relay stops hosting first.

Radio can host a Wi-Fi network for a phone running the [companion relay](../../companion/ios/README.md) and reach the internet over the phone's cellular data. Run `services/usb-diagnostics/host/moto-relay` from the Mac. It provisions a one-boot profile named `VioletRelay-XXXX` (WPA2-PSK) or, with `--open`, `VioletRelayOpen-XXXX`. It writes the SSID, passphrase and SOCKS password to `build/relay-pairing.json`, then asks Radio to host the network. A join request whose network name starts with `VioletRelay` hosts that network instead of joining one.

- **Access point.** [softap.rs](softap.rs) holds the gen4m access-point commands, following Motorola's driver: activate BSS 1 as a P2P-network BSS with own-MAC slot 1, request the channel, set the BSS information in AP mode, send the beacon template, and report the BSS as created.
- **Joining.** Radio answers probe, authentication and association frames itself, and admits one phone at a time.
- **WPA2.** [ap_wpa.c](ap_wpa.c) runs the 4-way handshake as the authenticator, using hostap's PTK, MIC and AES-wrap code.
- **IP.** [client.c](ip/client.c) serves `192.168.77.0/24`. It leases `192.168.77.2` with no router or DNS option, so the phone keeps cellular as its default route. Broadcasts go to the phone as unicast frames.
- **Relay.** The HTTPS test connects through SOCKS5 to the phone on port 1081, sends the hostname for the phone to resolve, and keeps TLS end to end on VioletOS.
- **Staying up.** The access point runs from Radio's event loop and outlives individual requests. If the phone leaves or re-associates, Radio drops only the IP state. While the network is up, every `moto-package join-wifi` repeats the HTTPS test.

Two supporting changes:
- **Radio's stack.** Radio now runs on a separate eight-page stack at `0x02600000`, as Storage does, so its package may use the full `0x3e000`-byte image window. Runtime keeps the `0x38000` limit.
- **Reboot.** The supervisor powers down the connectivity domain before a USB-requested reboot. Without that, a warm reset while hosting made slot B fail until the bootloader fell back to Android.

On November 22, 2026, the XT2163DL on slot B hosted the network and an iPhone 18 Pro Max (iOS 27.2) joined it. The Mac drove both devices with no hands on either.
- **Open network.** The phone joined `VioletRelayOpen-9034` and took the `192.168.77.2` lease. VioletOS fetched UUID `4b95ac2c-…` over verified HTTPS. The companion counted one connection, 4,044 bytes down and 454 up, leaving on its cellular interface `pdp_ip0`.
- **WPA2.** The phone joined `VioletRelay-9FE8`, completed the 4-way handshake and took the lease. VioletOS fetched `415d7130-…`, and ten further requests over the same session all succeeded. Each fresh TCP, SOCKS5, TLS and HTTPS exchange took 0.8–2.7 s, typically 1.1 s, as timed from the Mac including USB polling.
- **Not yet covered:** throughput measured from VioletOS, the phone locked or in the background, and a profile that persists across reboots.
