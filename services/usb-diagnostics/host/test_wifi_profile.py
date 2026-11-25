"""moto-wifi-profile on the Mac alone: what it sends the handset and what it saves.

A stand-in for build/moto-package records each request, so no phone is needed.
"""
from pathlib import Path
import json
import os
import pty
import shutil
import subprocess
import sys
import tempfile

script = Path(__file__).with_name("moto-wifi-profile")
MAC, NAME = "02:11:22:33:44:55", "Violet"
FIXED = bytes.fromhex(MAC.replace(":", "")) + NAME.encode()
ON = ["1", "yes", "true", "on", "enable", "enabled", "YES", " True "]
OFF = ["", "0", "no", "false", "off", "disable", "disabled", "No"]
# Records its request and what it read, and refuses join-options when told to.
TOOL = """#!/usr/bin/env python3
import os, sys
from pathlib import Path
data = sys.stdin.buffer.read()
with open(Path(__file__).with_name("log"), "a") as log:
    log.write(sys.argv[1] + " " + data.hex() + "\\n")
sys.exit(1 if sys.argv[1] == "join-options" and os.environ.get("REFUSE_OPTIONS") else 0)
"""


def run(root, *args, saved=None, **env):
    """Run the script; return its exit status, the join-options records it sent, and the saved profile."""
    log, profile = root / "build/log", root / "home/.config/violetos/wifi-profile.json"
    log.unlink(missing_ok=True)
    profile.unlink(missing_ok=True)
    if saved is not None:
        profile.parent.mkdir(parents=True, exist_ok=True)
        profile.write_text(json.dumps({"ssid": "net", "pmk": "00" * 32, **saved}))
    # The script asks its questions only of a terminal.
    terminal, stdin = pty.openpty()
    done = subprocess.run([sys.executable, root / "services/usb-diagnostics/host/moto-wifi-profile", *args],
                          env={"PATH": os.environ["PATH"], "HOME": str(root / "home"), **env},
                          stdin=stdin, capture_output=True, text=True, timeout=30)
    os.close(terminal)
    os.close(stdin)
    sent = [line.split(" ", 1) for line in log.read_text().splitlines()] if log.exists() else []
    options = [bytes.fromhex(data) for request, data in sent if request == "join-options"]
    return done.returncode, options, json.loads(profile.read_text()) if profile.exists() else None


def check(ok, what):
    if not ok:
        raise SystemExit("moto-wifi-profile: " + what)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        host = root / "services/usb-diagnostics/host"
        host.mkdir(parents=True)
        shutil.copy(script, host)
        (root / "build").mkdir()
        (root / "build/moto-package").write_text(TOOL)
        (root / "build/moto-package").chmod(0o755)
        kept = {"mac": MAC, "hostname": NAME}
        answers = {"VIOLET_SSID": "net", "VIOLET_WIFIPASS": "not-a-real-password"}
        given = {**answers, "VIOLET_FIXED_MAC": MAC, "VIOLET_HOSTNAME": NAME}

        status, options, saved = run(root, "--save", **answers)
        check(status == 0 and options == [] and set(saved) == {"ssid", "pmk"},
              "with nothing set, sent or kept a join setting")
        status, options, saved = run(root, "--save", **given)
        check(status == 0 and options == [FIXED] and saved["mac"] == MAC and saved["hostname"] == NAME,
              "did not send and keep the fixed address and host name")
        status, options, _ = run(root, "--saved", saved=kept)
        check(status == 0 and options == [FIXED], "did not send the kept fixed address and host name")

        for value in OFF:
            status, options, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT=value)
            check(status == 0 and options == [FIXED], f"VIOLET_IS_PRIVATE_CLIENT={value!r} was not taken as off")
        for value in ON:
            status, options, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT=value)
            check(status == 0 and options == [bytes(6)],
                  f"VIOLET_IS_PRIVATE_CLIENT={value!r} did not clear the kept address and name")
        for value in ("maybe", "2", "yes please"):
            status, options, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT=value)
            check(status != 0 and options == [], f"VIOLET_IS_PRIVATE_CLIENT={value!r} was not refused")

        status, options, saved = run(root, "--save", **given, VIOLET_IS_PRIVATE_CLIENT="yes")
        check(status == 0 and options == [bytes(6)] and set(saved) == {"ssid", "pmk"},
              "a private client sent or kept the fixed address and host name it was given")
        status, options, _ = run(root, "--saved", saved={}, VIOLET_IS_PRIVATE_CLIENT="1")
        check(status == 0 and options == [bytes(6)],
              "a private client did not clear settings from earlier in the boot")
        status, _, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT="1", REFUSE_OPTIONS="1")
        check(status != 0, "reported a private client although the handset refused the request")
    print(f"moto-wifi-profile sent and kept the right join settings; VIOLET_IS_PRIVATE_CLIENT read "
          f"{len(ON)} ways on and {len(OFF)} off, and overrode a fixed address and host name")


if __name__ == "__main__":
    main()
