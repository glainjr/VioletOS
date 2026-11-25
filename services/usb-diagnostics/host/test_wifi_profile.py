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
OFF = ["0", "no", "false", "off", "disable", "disabled", "No", " False "]
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
    """Run the script; return its exit status, the join-options records it sent, the saved profile
    and what it wrote to standard error."""
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
    return done.returncode, options, json.loads(profile.read_text()) if profile.exists() else None, done.stderr


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
        kept = {"mac": MAC, "hostname": NAME, "private": False}
        answers = {"VIOLET_SSID": "net", "VIOLET_WIFIPASS": "not-a-real-password"}
        given = {**answers, "VIOLET_FIXED_MAC": MAC, "VIOLET_HOSTNAME": NAME}
        clear, unused = [bytes(6)], "are not used"

        # Private unless told otherwise: nothing given is sent or kept.
        status, options, saved, said = run(root, "--save", **answers)
        check(status == 0 and options == clear and set(saved) == {"ssid", "pmk"} and unused not in said,
              "with nothing set, was not a private client")
        status, options, saved, said = run(root, "--save", **given)
        check(status == 0 and options == clear and set(saved) == {"ssid", "pmk"} and unused in said,
              "sent or kept a fixed address and host name without VIOLET_IS_PRIVATE_CLIENT=0, or did not say so")
        status, options, _, said = run(root, "--saved", saved={"mac": MAC, "hostname": NAME})
        check(status == 0 and options == clear and unused in said,
              "sent a kept address and name that were not kept with the choice to use them")

        # Told otherwise: the settings are sent and kept, with the choice.
        status, options, saved, _ = run(root, "--save", **answers, VIOLET_IS_PRIVATE_CLIENT="0")
        check(status == 0 and options == [] and saved["private"] is False and "mac" not in saved,
              "with only VIOLET_IS_PRIVATE_CLIENT=0, sent a join setting or did not keep the choice")
        status, options, saved, _ = run(root, "--save", **given, VIOLET_IS_PRIVATE_CLIENT="0")
        check(status == 0 and options == [FIXED] and saved == {**saved, **kept},
              "did not send and keep the fixed address and host name")
        status, options, _, _ = run(root, "--saved", saved=kept)
        check(status == 0 and options == [FIXED], "did not send the kept fixed address and host name")

        for value in OFF:
            status, options, _, _ = run(root, "--saved", saved={"mac": MAC, "hostname": NAME},
                                        VIOLET_IS_PRIVATE_CLIENT=value)
            check(status == 0 and options == [FIXED], f"VIOLET_IS_PRIVATE_CLIENT={value!r} was not taken as off")
        for value in ON:
            status, options, _, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT=value)
            check(status == 0 and options == clear,
                  f"VIOLET_IS_PRIVATE_CLIENT={value!r} did not clear the kept address and name")
        status, options, _, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT="")
        check(status == 0 and options == [FIXED], "an empty VIOLET_IS_PRIVATE_CLIENT overrode the kept choice")
        for value in ("maybe", "2", "yes please"):
            status, options, _, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT=value)
            check(status != 0 and options == [], f"VIOLET_IS_PRIVATE_CLIENT={value!r} was not refused")

        # A handset that refuses: fatal when privacy or a setting was asked for, not for the default.
        status, _, _, _ = run(root, "--saved", saved=kept, VIOLET_IS_PRIVATE_CLIENT="1", REFUSE_OPTIONS="1")
        check(status != 0, "reported a private client although the handset refused the request")
        status, _, _, _ = run(root, "--saved", saved=kept, REFUSE_OPTIONS="1")
        check(status != 0, "reported a fixed address although the handset refused it")
        status, _, _, said = run(root, "--saved", saved={}, REFUSE_OPTIONS="1")
        check(status == 0 and "did not confirm" in said,
              "the default stopped a join on a handset without the settings, or did not say so")
    print(f"moto-wifi-profile is a private client by default and sent and kept the right join settings; "
          f"VIOLET_IS_PRIVATE_CLIENT read {len(ON)} ways on and {len(OFF)} off")


if __name__ == "__main__":
    main()
