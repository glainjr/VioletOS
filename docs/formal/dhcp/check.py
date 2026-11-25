"""Check the DHCP client's models: the Lean proofs, then the TLA+ model with TLC.

Lean comes from elan (the toolchain is pinned in lean-toolchain). TLC is the
`tlc` command, or tla2tools.jar named by TLA2TOOLS and run with Java (JAVA, or
`java`). A tool that is missing counts as a failure, not a pass.
"""
from pathlib import Path
import os
import shutil
import subprocess
import sys

here = Path(__file__).resolve().parent
work = here.parents[2] / "build/formal/dhcp"
# Each TLC configuration, and the invariant it must find broken (None: no error at all).
CONFIGS = {"Phone": None, "Check": None, "CheckProxyArp": None, "NoCheckInUse": "NoDuplicateAddress"}


def lean():
    if not shutil.which("lean"):
        return "Lean is not installed (https://lean-lang.org/install)"
    run = subprocess.run(["lean", "DhcpClient.lean"], cwd=here, capture_output=True, text=True)
    output = (run.stdout + run.stderr).strip()
    if run.returncode or output:
        return output or f"lean exited with {run.returncode}"
    return None


def tlc_command():
    if os.environ.get("TLA2TOOLS"):
        jar = Path(os.environ["TLA2TOOLS"]).resolve()
        return [os.environ.get("JAVA", "java"), "-XX:+UseParallelGC", "-cp", str(jar), "tlc2.TLC"]
    return ["tlc"] if shutil.which("tlc") else None


def tlc(command, name, broken):
    # TLC leaves its state files beside the model, in a directory named for the
    # second it started: each configuration runs on a copy in a directory of its own.
    where = work / name
    shutil.rmtree(where, ignore_errors=True)
    where.mkdir(parents=True)
    for file in ("DhcpClient.tla", name + ".cfg"):
        shutil.copy(here / file, where / file)
    run = subprocess.run([*command, "-deadlock", "-config", name + ".cfg", "DhcpClient.tla"],
                         cwd=where, capture_output=True, text=True)
    output = run.stdout + run.stderr
    if broken:
        ok = f"Invariant {broken} is violated" in output
    else:
        ok = "No error has been found" in output
    return None if ok else output.strip()


def main():
    failed = False
    problem = lean()
    print("Lean  DhcpClient.lean:", problem or "every theorem holds")
    failed |= bool(problem)
    command = tlc_command()
    if not command:
        print("TLA+  not checked: put tlc on PATH, or set TLA2TOOLS to tla2tools.jar "
              "(https://github.com/tlaplus/tlaplus/releases)")
        failed = True
    for name, broken in CONFIGS.items() if command else ():
        problem = tlc(command, name, broken)
        expected = f"{broken} is violated, as it must be" if broken else "no error"
        print(f"TLA+  {name}.cfg:", problem or expected)
        failed |= bool(problem)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
