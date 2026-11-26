.DEFAULT_GOAL := moto
.PHONY: build moto emulator qemu-image test-emulator sim setup
PYTHON ?= python3
VENV := build/sel4/venv
build: moto
moto: setup
	$(PYTHON) tools/build_sel4.py moto
qemu-image: setup
	$(PYTHON) tools/build_sel4.py qemu
setup: $(VENV)/.deps
$(VENV)/.deps: tools/requirements-sel4.txt
	$(PYTHON) -m venv $(VENV)
	$(VENV)/bin/python -m pip install -r $<
	touch $@
build/sel4/viewer: tools/emulator/viewer.c
	mkdir -p $(@D)
	$(CC) $(VIEWER_FLAGS) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) $< -o $@ $(shell sdl2-config --libs)
VIEWER := build/sel4/viewer
ifeq ($(shell uname -s),Darwin)
VIEWER_FLAGS := -x objective-c
VIEWER := build/sel4/VioletOS.app/Contents/MacOS/viewer
$(VIEWER): build/sel4/viewer tools/emulator/Info.plist tools/emulator/VioletOS.icns
	mkdir -p $(@D)
	mkdir -p build/sel4/VioletOS.app/Contents/Resources
	cp tools/emulator/Info.plist build/sel4/VioletOS.app/Contents/
	cp tools/emulator/VioletOS.icns build/sel4/VioletOS.app/Contents/Resources/
	cp $< $@
endif
emulator: qemu-image $(VIEWER)
	$(PYTHON) tools/run.py $(VIEWER)
test-emulator: qemu-image
	@set -e; for mode in normal delayed drop-before drop-after stuck setup reset1 reset2; do \
	  $(PYTHON) targets/qemu/test_guest.py --spi $$mode --watchdog-enabled; \
	done
build/simulator: src/screen.c src/screen.h src/glyphs.h targets/mac-simulator/simulator.c
	mkdir -p $(@D)
	$(CC) -std=c11 -O2 -Wall -Wextra $(shell sdl2-config --cflags) -Isrc src/screen.c targets/mac-simulator/simulator.c -o $@ $(shell sdl2-config --libs)
sim: build/simulator
	./build/simulator

.PHONY: test-isolation component
test-isolation: setup
	VIOLET_TEST_ISOLATION=1 $(PYTHON) tools/build_sel4.py qemu
	@set -e; for region in touch storage dma scanout peer radio radio-dma; do \
	  $(PYTHON) targets/qemu/test_guest.py --probe $$region; \
	done

component: setup
	@test -n "$(NAME)" || (echo "Set NAME to the component name"; exit 1)
	VIOLET_COMPONENT_ONLY="$(NAME)" $(PYTHON) tools/build_sel4.py qemu

.PHONY: test-audio
test-audio: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/open-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_audio.py

# The test programs (Rust, C, Go, Python) live in targets/qemu/open-test-apps
# and go into this image only; they show up on Home as extra icons.
.PHONY: test-open
test-open: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/open-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_open.py

.PHONY: test-memory
test-memory: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath targets/qemu/memory-test-apps) $(PYTHON) tools/build_sel4.py qemu
	$(PYTHON) targets/qemu/test_memory.py

.PHONY: test-ladybird
test-ladybird: qemu-image
	$(PYTHON) targets/qemu/test_ladybird.py

.PHONY: test-wifi-app
test-wifi-app: qemu-image
	$(PYTHON) targets/qemu/test_wifi_app.py

# The voice agent: QR pairing, an encrypted install that survives a restart, reconnect
# and unpair, with the real Mac app (companion/mac) and a local relay.
test-agent:
	VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=violet-relay.test:3478 VIOLET_OPEN_NO_PYTHON=1 $(MAKE) qemu-image
	$(PYTHON) targets/qemu/test_agent.py

# Uses the downloaded Mac app and its installed models. Runs locally only.
LOCOMO_APP ?= /Applications/Locomo Music.app
.PHONY: test-locomo
test-locomo:
	VIOLET_AGENT_RELAY=http://violet-relay.test VIOLET_AGENT_STUN=off VIOLET_OPEN_SKIP=ladybird VIOLET_OPEN_NO_PYTHON=1 $(MAKE) qemu-image
	$(PYTHON) targets/qemu/test_locomo.py "$(LOCOMO_APP)"

.PHONY: test-open-apps
test-open-apps: qemu-image
	$(PYTHON) targets/qemu/test_open_apps.py

# Host unit tests (CI): each test finishes in well under a second.
HOST_CARGO ?= $(or $(shell command -v cargo),$(dir $(HOST_RUSTC))cargo)
.PHONY: test-unit
test-unit:
	@mkdir -p build/tests
	"$(HOST_RUSTC)" --edition=2021 --test -O services/audio/mic.rs -o build/tests/mic
	build/tests/mic
	@printf '#![no_std]\nextern crate alloc;\nextern crate std;\n#[path = "../../runtime/open/inflate.rs"]\nmod inflate;\n' > build/tests/inflate_test.rs
	"$(HOST_RUSTC)" --edition=2021 --test -O build/tests/inflate_test.rs -o build/tests/inflate
	build/tests/inflate
	python3 apps/lib/python/test_violet.py
	cd apps && RUSTC="$(abspath $(HOST_RUSTC))" "$(abspath $(HOST_CARGO))" test --release --locked --lib --bins -p violet -p home -p settings -p bitchat --target-dir ../build/host-tests

# The QEMU tests CI runs: one boot of the supervisor and its services (touch,
# the watchdog, Home answering a tap), then Home, Settings and Music across
# two boots (a setting kept in /data across the restart).
.PHONY: test-ci
test-ci: qemu-image
	$(PYTHON) targets/qemu/test_guest.py --spi normal --watchdog-enabled
	OPEN_FORCE_OFFLINE=1 $(PYTHON) targets/qemu/test_open_apps.py

.PHONY: test-moto-diagnostics
test-moto-diagnostics: moto qemu-image
	python3 targets/moto-g-pure/test_boot.py --diagnostics-trial confirm
	python3 targets/moto-g-pure/test_boot.py --diagnostics-trial fault
	python3 targets/moto-g-pure/test_boot.py --diagnostics-trial timeout

.PHONY: test-moto-ui
test-moto-ui: moto qemu-image
	$(PYTHON) targets/moto-g-pure/test_boot.py
	$(PYTHON) targets/moto-g-pure/test_boot.py --stall-services
	$(PYTHON) targets/moto-g-pure/test_ui.py

.PHONY: moto-debug
moto-debug: build/moto-debug
	@$<

build/moto-debug: services/usb-diagnostics/host/moto_debug.c
	@mkdir -p build
	$(CC) -Wall -Wextra -Werror -std=c11 $< $$(pkg-config --cflags --libs libusb-1.0) -o $@

# Which mode the connected handset is in. adb and fastboot come from PATH, or
# from the Android SDK's platform-tools when they are not on it.
.PHONY: showphone
PLATFORM_TOOLS ?= $(HOME)/Library/Android/sdk/platform-tools
showphone: build/moto-debug
	@export PATH="$$PATH:$(PLATFORM_TOOLS)"; \
	fastboot=$$(fastboot devices 2>/dev/null | awk 'NF>=2 {print $$1}'); \
	android=$$(adb devices 2>/dev/null | awk 'NF==2 {print $$1" ("$$2")"}'); \
	violet=$$($< 2>/dev/null >/dev/null && echo answering); \
	echo "Bootloader (fastboot): $${fastboot:-not found}"; \
	echo "Android (adb):         $${android:-not found}"; \
	echo "VioletOS (USB):        $${violet:-not found}"

.PHONY: test-moto-radio
MOTO_WIFI_STOCK ?= build/xt2163dl/S3RHS32.20-42-13-2-3-2/stock/wifi
HOST_RUSTC ?= $(or $(shell command -v rustc),build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc)
test-moto-radio: moto
	$(PYTHON) targets/moto-g-pure/test_supplicant.py
	"$(HOST_RUSTC)" --edition=2021 --test targets/moto-g-pure/test_history.rs -o build/sel4/moto/test-history
	build/sel4/moto/test-history
	$(CC) -std=c11 -Wall -Wextra -Werror targets/moto-g-pure/test_radio_memory.c -o build/sel4/moto/test-radio-memory
	build/sel4/moto/test-radio-memory
	VIOLET_TEST_WIFI_BUNDLE="$(abspath build/sel4/moto/wifi-firmware.bin)" VIOLET_TEST_WLAN_IMAGE="$(abspath $(MOTO_WIFI_STOCK)/WIFI_RAM_CODE_soc1_0_1_1.bin)" "$(HOST_RUSTC)" --edition=2021 --test targets/moto-g-pure/test_radio.rs -o build/sel4/moto/test-radio
	build/sel4/moto/test-radio

.PHONY: test-moto-ip-live
test-moto-ip-live: moto
	$(PYTHON) targets/moto-g-pure/test_ip_live.py

# moto-wifi-profile on the Mac alone, with a stand-in for build/moto-package:
# what it sends the handset and saves for each setting. Needs no phone.
.PHONY: test-moto-wifi-profile
test-moto-wifi-profile:
	$(PYTHON) services/usb-diagnostics/host/test_wifi_profile.py

# The handset's DHCP client against a scripted network on a virtual clock: no
# answer, a late offer, a refusal, a dead link, a lease that runs out.
# Needs no network and takes seconds.
.PHONY: test-moto-dhcp
test-moto-dhcp: moto
	$(PYTHON) targets/moto-g-pure/test_dhcp.py

# The DHCP client's models (docs/formal/dhcp): Lean proofs, and TLA+ checked by TLC.
.PHONY: check-dhcp-model
check-dhcp-model:
	$(PYTHON) docs/formal/dhcp/check.py

# A Mac bitchat peer for over-the-air tests of apps/open/bitchat. macOS keeps
# its Bluetooth grant across rebuilds while it is signed with one identity:
# BITCHAT_PEER_IDENTITY, by default the first Apple Development identity (else ad hoc).
.PHONY: bitchat-peer
bitchat-peer: build/BitchatPeer.app/Contents/MacOS/bitchat-peer
build/BitchatPeer.app/Contents/MacOS/bitchat-peer: tools/bitchat-peer/main.swift tools/bitchat-peer/Info.plist
	@mkdir -p $(@D)
	cp tools/bitchat-peer/Info.plist build/BitchatPeer.app/Contents/Info.plist
	swiftc -O $< -o $@
	codesign --force -s "$${BITCHAT_PEER_IDENTITY:-$$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/{print $$2; exit}')}" build/BitchatPeer.app 2>/dev/null || codesign --force -s - build/BitchatPeer.app

build/moto-package: services/usb-diagnostics/host/moto_package.c
	@mkdir -p build
	$(CC) -Wall -Wextra -Werror -std=c11 $< $$(pkg-config --cflags --libs libusb-1.0) -o $@

.PHONY: moto-component
moto-component: setup
	@test -n "$(NAME)" || (echo "Set NAME to the Moto component (e.g. radio)"; exit 1)
	VIOLET_COMPONENT_ONLY="$(NAME)" $(PYTHON) tools/build_sel4.py moto

# Violet Shield (docs/violet-shield-poc.md).
SHIELD_PY := build/shield/venv/bin/python
.PHONY: shield-setup shield-train test-shield-unit test-shield moto-shield
$(SHIELD_PY):
	$(PYTHON) -m venv build/shield/venv
	$(SHIELD_PY) -m pip install -q torch "transformers>=5.24" sentence-transformers coremltools numpy cryptography pillow
shield-setup: $(SHIELD_PY)
shield-train: $(SHIELD_PY)
	$(SHIELD_PY) tools/shield-trainer/train.py
test-shield-unit: $(SHIELD_PY)
	SHIELD_PYTHON=$(SHIELD_PY) HOST_RUSTC="$(HOST_RUSTC)" targets/qemu/test_shield_unit.sh
test-shield: qemu-image $(SHIELD_PY)
	$(SHIELD_PY) targets/qemu/test_shield.py
# Moto image with the demo apps for use with the Mac companion.
moto-shield: setup
	VIOLET_OPEN_EXTRA_APPS=$(abspath apps/shield-demo) $(PYTHON) tools/build_sel4.py moto
