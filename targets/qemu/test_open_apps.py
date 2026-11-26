"""Home, Settings and Music in QEMU, driven by touch.

First boot: Home comes up by itself; its Settings icon opens Settings, where
Date & Time picks Tokyo (a shared setting kept in /data), the Wi-Fi page scans
the virtual networks, and Agent Apps opens. Back on Home, Music opens and
starts (when the image has it). Second boot on the same disk: Settings starts
with Tokyo still chosen.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
ICONS = json.loads(os.environ["OPEN_ICONS"])
STAGE = os.environ["OPEN_STAGE"]

def text():
    n = struct.unpack_from("<I", log, 4)[0]
    data = bytes(log[8:4096])
    if n <= len(data):
        return data[:n].decode(errors="replace")
    s = n % len(data)
    return (data[s:] + data[:s]).decode(errors="replace")
seen = ""
def everything():
    global seen
    t = text()
    for k in range(min(len(seen), len(t)), 0, -1):
        if seen.endswith(t[:k]):
            seen += t[k:]
            return seen
    seen += t
    return seen
def fail(why):
    print("open apps test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def count(line):
    return everything().count(line)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.5)
def menu(i):
    # Settings' menu rows (apps/open/settings/src/pages.rs).
    tap(360, 250 + i * 124 + 54)
def back():
    tap(100, 60)
def back_to_menu(what):
    before = count("settings: Menu")
    back()
    until(what, lambda: count("settings: Menu") > before)

until("Home starts", lambda: "home: ready" in everything(), 90)
time.sleep(2)
tap(*ICONS["settings"])
until("Settings starts", lambda: "settings: ready" in everything(), 60)
if STAGE == "second":
    if "settings: ready, city=Tokyo" not in everything():
        fail("the city chosen before the restart was lost")
    print("settings: Tokyo kept across the restart", file=sys.stderr)
    sys.exit(0)
time.sleep(1)
menu(2)  # Radios
until("Radios opens", lambda: "settings: Radios" in everything())
back_to_menu("back on the menu from Radios")
menu(4)  # Date & Time
until("Date & Time opens", lambda: "settings: Time" in everything())
tap(195, 250 + 8 * 118 + 51)  # Tokyo, the left column's ninth row
until("Tokyo chosen", lambda: "settings: time city Tokyo" in everything())
back_to_menu("back on the menu")
menu(0)  # Wi-Fi
until("Wi-Fi scans", lambda: "wifi: found" in everything(), 30)
print("settings:", [l for l in everything().splitlines() if "wifi: found" in l][-1], file=sys.stderr)
back_to_menu("back on the menu again")
menu(5)  # Agent Apps
until("Agent Apps opens", lambda: "settings: AgentApps" in everything())
back_to_menu("back on the menu once more")
back()  # ‹ Home
time.sleep(1)
if "music" in ICONS:
    tap(*ICONS["music"])
    until("Music starts", lambda: "music: starting" in everything(), 60)
    print("music: started from Home", file=sys.stderr)
time.sleep(2)  # the runtime writes /data back on its next pass
'''


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    viewer.write_text(SCRIPT)
    viewer.chmod(0o755)
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make qemu-image).")
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    apps = home_grid.apps((original / "initfs.cpio").read_bytes())
    icons = {name: home_grid.icon(i, apps) for i, name in enumerate(apps)}
    os.environ["OPEN_ICONS"] = json.dumps(icons)
    for stage in ("first", "second"):
        os.environ["OPEN_STAGE"] = stage
        status = run.main(str(viewer))
        if status != 0:
            raise SystemExit(f"open apps test failed in the {stage} boot (viewer status {status})")
    print("Open apps verified: Home, Settings (a setting kept across a restart, Wi-Fi, Agent Apps)" + (", Music" if "music" in icons else ""))
