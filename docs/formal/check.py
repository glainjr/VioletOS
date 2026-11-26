"""Check one of the models under docs/formal: its Lean proofs, then its TLA+ model with TLC.

    check.py dhcp     the DHCP client (docs/formal/dhcp)
    check.py stack    the Wi-Fi path from radio power to an open TCP connection (docs/formal/stack)

Lean comes from elan (the toolchain is pinned in each directory's lean-toolchain).
TLC is the `tlc` command, or tla2tools.jar named by TLA2TOOLS and run with Java
(JAVA, or `java`). A tool that is missing counts as a failure, not a pass.
"""
from pathlib import Path
import os
import shutil
import subprocess
import sys

# For each model: its Lean and TLA+ file name, then each TLC configuration and
# the invariant it must find broken (None: no error at all).
MODELS = {
    "dhcp": ("DhcpClient", {"Phone": None, "Check": None, "CheckProxyArp": None,
                            "NoCheckInUse": "NoDuplicateAddress"}),
    "stack": ("WifiStack", {"Ideal": None, "FarAp": None, "OneLoss": None, "OneLossTwoAps": None,
                            "WrongKey": None, "Rough": None,
                            "OneLossFails": "ReachableMeansJoined", "Unseen": "ReachableMeansJoined",
                            "MissedBeacon": "ReachableMeansJoined", "Deauth": "NoAddressMeansWaited"}),
}


def lean(here, module):
    if not shutil.which("lean"):
        return "Lean is not installed (https://lean-lang.org/install)"
    run = subprocess.run(["lean", module + ".lean"], cwd=here, capture_output=True, text=True)
    output = (run.stdout + run.stderr).strip()
    if run.returncode or output:
        return output or f"lean exited with {run.returncode}"
    return None


def tlc_command():
    if os.environ.get("TLA2TOOLS"):
        jar = Path(os.environ["TLA2TOOLS"]).resolve()
        return [os.environ.get("JAVA", "java"), "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC"]
    return ["tlc"] if shutil.which("tlc") else None


def tlc(command, here, work, module, name, broken):
    # TLC leaves its state files beside the model, in a directory named for the
    # second it started: each configuration runs on a copy in a directory of its own.
    where = work / name
    shutil.rmtree(where, ignore_errors=True)
    where.mkdir(parents=True)
    for file in (module + ".tla", name + ".cfg"):
        shutil.copy(here / file, where / file)
    run = subprocess.run([*command, "-deadlock", "-config", name + ".cfg", module + ".tla"],
                         cwd=where, capture_output=True, text=True)
    output = run.stdout + run.stderr
    if broken:
        ok = f"Invariant {broken} is violated" in output
    else:
        ok = "No error has been found" in output
    return None if ok else output.strip()


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in MODELS:
        raise SystemExit("usage: check.py " + " | ".join(MODELS))
    module, configs = MODELS[sys.argv[1]]
    here = Path(__file__).resolve().parent / sys.argv[1]
    work = here.parents[2] / "build/formal" / sys.argv[1]
    failed = False
    problem = lean(here, module)
    print(f"Lean  {module}.lean:", problem or "every theorem holds")
    failed |= bool(problem)
    command = tlc_command()
    if not command:
        print("TLA+  not checked: put tlc on PATH, or set TLA2TOOLS to tla2tools.jar "
              "(https://github.com/tlaplus/tlaplus/releases)")
        failed = True
    for name, broken in configs.items() if command else ():
        problem = tlc(command, here, work, module, name, broken)
        expected = f"{broken} is violated, as it must be" if broken else "no error"
        print(f"TLA+  {name}.cfg:", problem or expected)
        failed |= bool(problem)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
