"""Link the VioletOS first task and package it with the target kernel."""
from pathlib import Path
import sys, subprocess, struct, shlex, hashlib, os, shutil

here = Path(__file__).resolve().parent
repo = here.parents[2]
build = repo / "build/sel4"
variant, stock_path = sys.argv[1:]
platform_include = here.parent / "include"
out = build / variant
def run(cmd):
    subprocess.run(cmd, cwd=out, check=True)

abi = out/"kernel"
probe = os.environ.get("VIOLET_TEST_ISOLATION") == "1" and variant=="qemu"
if probe:
    out=out/"isolation-test"
    out.mkdir(exist_ok=True)
    shutil.copyfile(out.parent/"handoff.dtb",out/"handoff.dtb")
line = next(s for s in subprocess.check_output(["ninja","-C",abi,"-t","commands","sel4"],text=True).splitlines() if " -c " in s and s.endswith("/sel4_bootinfo.c"))
args = shlex.split(line)
# seL4's toolchain file runs the compiler through ccache when it is installed.
cc = Path(next(a for a in args if Path(a).name.startswith("clang")))
ld = cc.with_name("ld.lld")
objcopy = cc.with_name("llvm-objcopy")
common = [cc,"--target=arm-none-eabi","-mcpu=cortex-a53","-marm","-mfloat-abi=soft","-Oz","-ffreestanding","-fno-builtin","-fno-stack-protector","-fno-unwind-tables","-fno-pic","-mno-unaligned-access","-ffunction-sections","-fdata-sections"]
incs = [a for a in args if a.startswith("-I") and ("libsel4" in a or "autoconf" in a or "gen_config" in a)]
# Cores the kernel boots (kernel/sel4/build.py); the loader starts the others.
defines = ["-DNDEBUG", "-DROOT_BYTES=0x100000", "-DVIOLET_CORES=" + os.environ.get("VIOLET_CORES", "1")]
if probe: defines.append("-DVIOLET_TEST_ISOLATION")
defines.append("-DVIOLET_QEMU" if variant=="qemu" else "-DVIOLET_MOTO")
sys.path.insert(0,str(repo / "services/touch"))
from firmware import prepare
prepare(out, variant, Path(stock_path).parent)
sources = [here/"root.S", here/"strings.c"]
for source in sources:
    obj = source.name+".o"
    run(common + incs + defines + ["-I",out,"-I",platform_include,"-std=gnu11","-c",source,"-o",obj])
rustc = os.environ.get("RUSTC") or shutil.which("rustc")
if not rustc:
    local = repo/"build/spike1/rustup/toolchains/1.91.1-aarch64-apple-darwin/bin/rustc"
    if not local.exists():
        sys.exit("Install Rust and run rustup target add armv7a-none-eabi")
    rustc = local
os.environ["VIOLET_FIRMWARE_RS"] = str(out/"firmware.rs")
sys.path.insert(0,str(repo/"image/packages"))
os.environ["VIOLET_CATALOG_VOLUME_RS"] = str(repo/"system/package-manager/catalog_volume.rs")
from build import components
# Only process-local ABI shims, never the root allocator, enter packages.
board_spi = repo/("targets/qemu/fixture.c" if variant=="qemu" else "targets/moto-g-pure/spi_mmio.c")
shims=[]
for source in [repo/"services/touch/events.c", repo/"services/storage/storage_ipc.c", here.parent/"board/watchdog.c", board_spi, here/"strings.c"]:
    obj=out/("component-"+source.name+".o")
    run(common+incs+defines+["-DVIOLET_COMPONENT","-I",out,"-I",platform_include,"-std=gnu11","-c",source,"-o",obj])
    shims.append(obj)
only=os.environ.get("VIOLET_COMPONENT_ONLY")
selected=None if variant=="qemu" else {"launcher","runtime","touch","display","storage","power","radio","usb-diagnostics","audio","open","camera"}
storage_backend=repo/"targets/moto-g-pure/emmc.rs" if variant=="moto" else None
components(repo,out,cc,common,incs,rustc,shims,only,selected,storage_backend,variant)
if only or (variant=="qemu" and os.environ.get("VIOLET_APP_DIR")): sys.exit(0)
(out/"catalog.S").write_text('.section .rodata.catalog,"a"\n.balign 4\n.global builtin_catalog,builtin_catalog_end\nbuiltin_catalog: .incbin "catalog.bin"\nbuiltin_catalog_end:\n.balign 4\n.global recovery_manager\nrecovery_manager: .incbin "packages/package_manager.vpk"\n')
run(common+["-c",out/"catalog.S","-o","catalog.o"])
signing_objects = []
if variant == "moto":
    # Only the public key enters the image. Reuse the private key on this Mac.
    from tempfile import TemporaryDirectory
    key = out / "developer-signing.pem"
    if not key.exists():
        with TemporaryDirectory(dir=out) as directory:
            temporary = Path(directory) / "key.pem"
            run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", temporary])
            temporary.chmod(0o600)
            temporary.replace(key)
    public = subprocess.check_output(["openssl", "ec", "-in", key, "-pubout", "-outform", "DER",
                                      "-conv_form", "uncompressed"], stderr=subprocess.DEVNULL)
    prefix = bytes.fromhex("3059301306072a8648ce3d020106082a8648ce3d030107034200")
    if len(public) != len(prefix) + 65 or not public.startswith(prefix + b"\x04"):
        sys.exit("Expected a P-256 developer key")
    (out / "developer-key.h").write_text("static unsigned char developer_key[] = {" +
        ",".join(map(str, public[len(prefix):])) + "};\n")
    sys.path.insert(0, str(repo / "services/radio"))
    from build_tls import build as build_crypto
    signing_objects = [build_crypto(repo, out, common)[1]]
    incs += ["-I", build / "bearssl-0.6/inc", "-I", repo / "services/radio/freestanding"]
run(common+incs+defines+["-I",out,"-I",platform_include,"-std=gnu11","-c",repo/"system/supervisor/supervisor.c","-o","supervisor.o"])
root_objects=["root.S.o","supervisor.o","strings.c.o","catalog.o"]
if variant=="qemu":
    run([rustc,"--edition=2021","--target=armv7a-none-eabi","--crate-type=staticlib","--cfg","package_volume","-Cpanic=abort","-Copt-level=s",repo/"system/package-manager/volume.rs","-o","volume.a"])
    root_objects.insert(2,"volume.a")
else:
    run(common+incs+defines+["-I",platform_include,"-c",board_spi,"-o","board-map.o"])
    root_objects.append("board-map.o")
    for name, flags in [("loader-history.a", ["--cfg", "loader_trace", "--cfg", "package_catalog"]), ("root-history.a", ["--cfg", "package_catalog"])]:
        run([rustc, "--edition=2021", "--target=armv7a-none-eabi", "--crate-type=staticlib",
             "--cfg", "boot_trace", *flags, "-Cpanic=abort", "-Copt-level=s",
             repo/"targets/moto-g-pure/boot_trace.rs", "-o", name])
    root_objects.extend(["root-history.a", *signing_objects])
(out/"root.ld").write_text("ENTRY(_start)\nSECTIONS { . = 0x100000; .image : { *(.entry) *(.text*) *(.rodata*) . = ALIGN(4096); writable_start = .; *(.data*) *(.bss*) image_end = .; } /DISCARD/ : { *(.ARM.exidx*) } }")
run([ld,"--gc-sections","-T","root.ld",*root_objects,"-o","root.elf"])
run([objcopy,"-O","binary","root.elf","root.bin"])
assert (out/"root.bin").stat().st_size < 0xfe000
elf = (abi/"kernel.elf").read_bytes()
assert elf[:6] == b"\x7fELF\x01\x01" and struct.unpack_from("<I",elf,24)[0] == 0xe0000000
phoff, = struct.unpack_from("<I",elf,28)
size,count = struct.unpack_from("<HH",elf,42)
image = bytearray()
for i in range(count):
    kind,offset,va,pa,filesz,memsz,flags,align = struct.unpack_from("<8I",elf,phoff+i*size)
    if kind != 1: continue
    assert va-pa == 0xa0000000 and 0x40000000 <= pa <= pa+memsz <= 0x40100000 and filesz <= memsz
    index = pa-0x40000000
    if len(image)<index+memsz: image.extend(bytes(index+memsz-len(image)))
    image[index:index+filesz]=elf[offset:offset+filesz]
(out/"kernel.bin").write_bytes(image+bytes(-len(image)%4))
(out/"payload.S").write_text('.section .payload,"a"\n.global kernel,kernel_end,root,root_end\nkernel: .incbin "kernel.bin"\nkernel_end:\nroot: .incbin "root.bin"\nroot_end:\n')
# Open apps' file system image: QEMU loads it at 0x4a000000, the Moto loader carries it.
sys.path.insert(0,str(repo/"tools"))
from build_initfs import main as build_initfs
# The handset carries it gzip-compressed as its boot image's ramdisk.
from build_initfs import HANDSET_SKIP
build_initfs(out/"initfs.cpio", compress=variant=="moto", **(dict(python=False, skip=HANDSET_SKIP) if variant=="moto" else {}))
if variant == "moto":
    sys.path.insert(0, str(repo/"targets/moto-g-pure"))
    from wifi_firmware import prepare_wifi
    prepare_wifi(Path(stock_path).parent, out/"wifi-firmware.bin")
    with (out/"payload.S").open("a") as payload:
        payload.write('.balign 4\n.global radio_firmware,radio_firmware_end\nradio_firmware: .incbin "wifi-firmware.bin"\nradio_firmware_end:\n')
fdt = build/"dtc-1.7.2/libfdt"
ndk = cc.parent.parent/"sysroot/usr/include"
headers = ["-I"+str(fdt),"-isystem",ndk/"arm-linux-androideabi","-isystem",ndk]
objects = []
for source in [repo/"boot/loader/loader.S", repo/"boot/loader/loader.c", here/"strings.c", out/"payload.S"]+[fdt/(n+".c") for n in ("fdt","fdt_ro","fdt_check","fdt_addresses")]:
    obj = source.name+".o"
    run(common+defines+headers+["-c",source,"-o",obj])
    objects.append(obj)
if variant == "moto": objects.append("loader-history.a")
run([ld,"--gc-sections","-T",repo/"boot/loader/loader.ld",*objects,"-o","loader.elf"])
run([objcopy,"-O","binary","loader.elf","zImage"])
z=(out/"zImage").read_bytes()
assert struct.unpack_from("<3I",z,36)==(0x016f2818,0,len(z))
if variant=="qemu":
    run(common+["-DIMAGE_BYTES="+str(len(z)),"-c",here/"qemu-entry.S","-o","qemu-entry.o"])
    run([objcopy,"-O","binary","-j",".text","qemu-entry.o","entry.bin"])
if variant=="moto":
    sys.path.insert(0,str(repo/"tools"))
    from phone_image import rebuild_v2
    stock=Path(stock_path).read_bytes()
    assert hashlib.sha256(stock).hexdigest()=="36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a"
    # The bootloader loads the ramdisk; VioletOS's loader copies it to the open apps' window.
    image=rebuild_v2(stock,(out/"initfs.cpio").read_bytes(),kernel=z)
    # Only the 1 MiB after this explicit slot-B marker belongs to the filesystem.
    marker_offset=16*1024**2
    if len(image)>marker_offset:raise ValueError("boot payload overlaps the storage window")
    marker=b"VioletOS XT2163DL boot_b storage v1\n".ljust(512,b"\0")
    image=image.ljust(marker_offset,b"\0")+marker+bytes(1024**2)
    image += b"VioletOS boot history v1\n".ljust(512,b"\0") + bytes(1024)
    image += b"VioletOS catalog banks v1\n".ljust(512,b"\0") + bytes(4098*512)
    (out/"boot.img").write_bytes(image)
    page, = struct.unpack_from("<I", stock, 36)
    assert image[page:page+len(z)]==z
    print(hashlib.sha256(image).hexdigest(),out/"boot.img")
