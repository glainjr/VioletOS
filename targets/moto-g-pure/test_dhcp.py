"""Run the Moto DHCP client against a scripted network on a virtual clock."""
from pathlib import Path
import subprocess
from build_ip import sources

here = Path(__file__).resolve().parent
build = here.parents[1] / "build/sel4"
src = build / "lwip-STABLE-2_2_1_RELEASE/src"
exe = build / "moto/test-dhcp"
subprocess.run(["cc", "-O2", "-Wall", "-I", here / "ip", "-I", src / "include",
                *sources(src, here / "ip"), here / "ip/test_dhcp.c", "-o", exe], check=True)
subprocess.run([exe], check=True, timeout=40)
