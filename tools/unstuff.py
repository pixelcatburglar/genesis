"""Index and extract the build-1000 client's .stuff archives.

Format (little-endian):
    u32 count
    count x { u32 size; u32 namelen; char name[namelen]; u8 nul }
    file bodies, concatenated in directory order
    16-byte trailer: 00 00 00 00 "EmbedFs 1.0" 00

usage: py -3 -I tools/unstuff.py <EVE dir> <out dir> [--index-only ARCHIVE ...]
Writes <out>/index.jsonl (every entry of every archive) and extracts each file
to <out>/<name with backslashes as slashes>, skipping archives listed after
--index-only.
"""
import glob
import json
import os
import struct
import sys

TRAILER = b'\0\0\0\0EmbedFs 1.0\0'


def read_dir(f):
    (count,) = struct.unpack('<I', f.read(4))
    entries = []
    for _ in range(count):
        size, namelen = struct.unpack('<II', f.read(8))
        name = f.read(namelen + 1).rstrip(b'\0').decode('latin-1')
        entries.append((name, size))
    return entries


def main():
    eve, out = sys.argv[1], sys.argv[2]
    index_only = set(a.lower() for a in sys.argv[4:]) if '--index-only' in sys.argv else set()
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, 'index.jsonl'), 'w') as idx:
        for archive in sorted(glob.glob(os.path.join(eve, '*.stuff'))):
            base = os.path.basename(archive)
            with open(archive, 'rb') as f:
                entries = read_dir(f)
                offset = f.tell()
                extract = base.lower() not in index_only
                for name, size in entries:
                    idx.write(json.dumps({'archive': base, 'path': name.replace('\\', '/'),
                                          'offset': offset, 'size': size}) + '\n')
                    if extract:
                        f.seek(offset)
                        dest = os.path.join(out, *name.split('\\'))
                        os.makedirs(os.path.dirname(dest), exist_ok=True)
                        with open(dest, 'wb') as d:
                            d.write(f.read(size))
                    offset += size
                f.seek(offset)
                assert f.read() == TRAILER, (base, offset)
            print(f'{base}: {len(entries)} entries{"" if extract else " (indexed only)"}')


if __name__ == '__main__':
    main()
