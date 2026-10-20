"""Extract a blue class's member table from a decompiled static initializer.

Blue classes register their members in a table of fixed-stride entries built
at runtime by a static initializer (destiny.dll FUN_10009f50 for the ball
class). Each entry is 0x38 bytes:
    +0x10 char *name   +0x14 VARTYPE   +0x18 offset in the C++ object
    +0x1c size         +0x24 char *doc +0x28 flags
This reads the initializer's `_DAT_<addr> = <value>;` stores from the Ghidra
output, resolves name pointers against the DLL image, and prints the table.

usage: py -3 -I tools/destiny_members.py <dll> <decompiled.c> <initializer> <first name addr>
   e.g. ... EVE/bin/destiny.dll decompiled/native/destiny.dll.c FUN_10009f50 0x10057228
"""
import json
import re
import struct
import sys

import pefile

dll, c_file, func, first = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4], 0)
pe = pefile.PE(dll)
base = pe.OPTIONAL_HEADER.ImageBase
image = pe.get_memory_mapped_image()

src = open(c_file, encoding='utf-8', errors='replace').read()
start = src.index(f'// ==== {func} @')
end = src.index('// ==== ', start + 10)
body = src[start:end]
stores = {}
for m in re.finditer(r'_?DAT_([0-9a-f]{8}) = ([^;]+);', body):
    stores[int(m.group(1), 16)] = m.group(2).strip()


def cstr(va):
    o = va - base
    return image[o:image.index(b'\0', o)].decode('latin-1')


def value(v):
    if v is None:
        return None
    if v.startswith('"'):
        return v.strip('"')
    m = re.match(r'&(?:PTR_)?(?:s_\w+_)?DAT_([0-9a-f]{8})|&PTR_s_\w+_([0-9a-f]{8})', v)
    if m:
        return cstr(int(m.group(1) or m.group(2), 16))
    try:
        return int(v, 0)
    except ValueError:
        return v


members = []
k = 0
while True:
    name = value(stores.get(first + 0x38 * k))
    if name is None:
        break
    members.append({
        'name': name,
        'vartype': value(stores.get(first + 0x38 * k + 4)),
        'offset': value(stores.get(first + 0x38 * k + 8)),
        'size': value(stores.get(first + 0x38 * k + 12)),
        'doc': value(stores.get(first + 0x38 * k + 0x14)),
        'flags': value(stores.get(first + 0x38 * k + 0x18)),
    })
    k += 1
json.dump(members, sys.stdout, indent=1)
