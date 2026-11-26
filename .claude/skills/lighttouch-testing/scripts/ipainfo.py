#!/usr/bin/env python3
"""What an IPA needs: bundle ID, version, minimum iOS, device family and the CPU slices of its executable.

usage: ipainfo.py FILE.ipa [...]   (run with python3 -I on downloaded files)

armv6-only apps don't run on the iPhone 5 (iOS 10); armv7-only apps don't run on the iPod touch 2G; family 2 alone is
iPad-only. An executable that won't parse (FairPlay "encrypted" copies still parse) is reported as such.
"""
import plistlib
import struct
import sys
import zipfile

CPU = {(12, 6): "armv6", (12, 9): "armv7", (12, 11): "armv7s", (16777228, 0): "arm64", (16777228, 1): "arm64"}


def slices(data):
    magic = struct.unpack(">I", data[:4])[0]
    if magic in (0xCAFEBABE, 0xCAFEBABF):
        n = struct.unpack(">I", data[4:8])[0]
        size = 32 if magic == 0xCAFEBABF else 20
        archs = [struct.unpack(">II", data[8 + size * i:16 + size * i]) for i in range(n)]
    elif struct.unpack("<I", data[:4])[0] in (0xFEEDFACE, 0xFEEDFACF):
        archs = [struct.unpack("<II", data[4:12])]
    else:
        return ["?"]
    return [CPU.get((c, s & 0xFFFFFF), f"{c}/{s}") for c, s in archs]


for path in sys.argv[1:]:
    try:
        z = zipfile.ZipFile(path)
        plist = next(n for n in z.namelist() if n.count("/") == 2 and n.endswith(".app/Info.plist"))
        info = plistlib.loads(z.read(plist))
        exe = plist.rsplit("/", 1)[0] + "/" + info.get("CFBundleExecutable", "")
        archs = slices(z.read(exe)) if exe in z.namelist() else ["no executable"]
        print(f"{info.get('CFBundleIdentifier')}\t{info.get('CFBundleShortVersionString') or info.get('CFBundleVersion')}"
              f"\tmin iOS {info.get('MinimumOSVersion', '?')}\tfamily {info.get('UIDeviceFamily', [1])}"
              f"\t{','.join(archs)}\t{path}")
    except Exception as e:  # a damaged or non-IPA file: say so and go on
        print(f"?\t\t\t\t{e}\t{path}")
