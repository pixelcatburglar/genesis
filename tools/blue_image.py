"""Reader for blue object images ("Blue\\0" version 7), the format of every
.blue resource and of marshal tag 0x18 (a blue object inside a marshal stream).

Layout (little-endian; reversed from blue.dll's v7 reader, vtable 0x10136d40,
and verified by parsing every .blue file in the client's archives):

    0x00  "Blue\\0"
    0x05  u32  version (7)
    0x09  CLSID of the root object: module[10] class[24] u16 classVersion
    0x2d  u32  total image size
    0x31  u64  FILETIME saved
    0x39  u32  object count (= share-length table entries)
    0x3d  u32, u32, u8[3]          unknown, zero in shipped files
    0x48  u32  body length
    0x4c  u32  string count
    0x50  u32  total string bytes (without length prefixes)
    0x54  u32  class count
    0x58  u32  object count
    0x5c  u32  (see HEADER_FIELDS below)
    0x60  u32  (see HEADER_FIELDS below)
    0x64  body: one object reference (the root), objects inline
    ....  class table: per class u16 nameStr, u16 moduleStr, u32 version,
          u32 memberCount, memberCount x (u16 nameStr, u8 vartype)
    ....  share-length table: u32 per object = its serialised length + 2
    ....  string table: per string u32 length + bytes

Object reference: u8 tag. 0x05 = inline: u16 shareIdx, then the object
(u8 classIdx, records). 0x6a = None. Others (0x06, 0x64, 0x69) are handled
by the reader but do not occur in what we write.
Object: u8 classIdx, then records {u8 gap, value}: the member index is
previous + gap + 1 (from -1); 0xff ends. Classes that have Python state then
carry one marker byte (0xd3 = no state).
Value widths by VARTYPE: 1 int32, 2 float32, 3 float64, 4 bool(u8),
5/6 object reference, 8/12 u16 string index (0xffff = None).

usage: py -3 -I tools/blue_image.py FILE.blue...    (prints a summary / errors)
"""
import struct
import sys

VT_WIDTH = {1: 4, 2: 4, 3: 8, 4: 1, 8: 2, 12: 2}
VT_OBJECT = (5, 6)


class BlueImageError(Exception):
    pass


def parse(data):
    if data[:5] != b'Blue\0':
        raise BlueImageError('not a blue image')
    version = struct.unpack_from('<I', data, 5)[0]
    if version != 7:
        raise BlueImageError(f'version {version}')
    hdr = {
        'module': data[9:19].rstrip(b'\0').decode('latin-1'),
        'class': data[19:43].rstrip(b'\0').decode('latin-1'),
        'classVersion': struct.unpack_from('<H', data, 43)[0],
        'size': struct.unpack_from('<I', data, 0x2d)[0],
        'u39': struct.unpack_from('<I', data, 0x39)[0],
        'u3d': data[0x3d:0x48].hex(),
    }
    (hdr['bodyLen'], hdr['stringCount'], hdr['stringBytes'], hdr['classCount'], hdr['objectCount'],
     hdr['u5c'], hdr['u60']) = struct.unpack_from('<7I', data, 0x48)
    body_start, body_end = 0x64, 0x64 + hdr['bodyLen']

    # classes
    pos = body_end
    classes = []
    for _ in range(hdr['classCount']):
        name, module, ver, n = struct.unpack_from('<HHII', data, pos)
        pos += 12
        members = []
        for _ in range(n):
            mname, vt = struct.unpack_from('<HB', data, pos)
            pos += 3
            members.append((mname, vt))
        classes.append({'name': name, 'module': module, 'version': ver, 'members': members})
    share_lengths = list(struct.unpack_from(f'<{hdr["objectCount"]}I', data, pos))
    pos += 4 * hdr['objectCount']
    strings = []
    for _ in range(hdr['stringCount']):
        (n,) = struct.unpack_from('<I', data, pos)
        strings.append(data[pos + 4:pos + 4 + n].decode('latin-1'))
        pos += 4 + n
    if pos != len(data):
        raise BlueImageError(f'trailing {len(data) - pos} bytes after string table')
    if sum(len(s) for s in strings) != hdr['stringBytes']:
        raise BlueImageError('string byte total mismatch')
    for c in classes:
        c['name'] = strings[c['name']]
        c['module'] = strings[c['module']]
        c['members'] = [(strings[m], vt) for m, vt in c['members']]

    objects = {}

    def read_ref(p):
        tag = data[p]
        if tag == 0x6a:
            return None, p + 1
        if tag != 0x05:
            raise BlueImageError(f'object tag {tag:#x} at {p:#x}')
        (share,) = struct.unpack_from('<H', data, p + 1)
        start = p + 3
        obj, p = read_object(start)
        if share < len(share_lengths) and share_lengths[share] - 2 != p - start:
            raise BlueImageError(f'share {share} length {share_lengths[share] - 2} != {p - start}')
        objects[share] = obj
        return obj, p

    def read_object(p):
        cls = classes[data[p]]
        p += 1
        values = {}
        idx = -1
        while True:
            gap = data[p]
            p += 1
            if gap == 0xff:
                break
            idx += gap + 1
            name, vt = cls['members'][idx]
            if vt in VT_OBJECT:
                values[name], p = read_ref(p)
            elif vt in (8, 12):
                (si,) = struct.unpack_from('<H', data, p)
                values[name] = None if si == 0xffff else strings[si]
                p += 2
            elif vt in VT_WIDTH:
                w = VT_WIDTH[vt]
                fmt = {1: '<i', 2: '<f', 3: '<d', 4: '<B'}[vt]
                values[name] = struct.unpack_from(fmt, data, p)[0]
                p += w
            else:
                raise BlueImageError(f'unsupported vartype {vt} for {cls["name"]}.{name}')
        if p < body_end and data[p] in (0xd2, 0xd3):
            if data[p] == 0xd2:
                raise BlueImageError('python state section (0xd2) not supported')
            p += 1
        return {'__class__': f'{cls["module"]}.{cls["name"]}', **values}, p

    root, p = read_ref(body_start)
    if p != body_end:
        raise BlueImageError(f'body ended at {p:#x}, expected {body_end:#x}')
    return hdr, classes, root


if __name__ == '__main__':
    for path in sys.argv[1:]:
        try:
            hdr, classes, root = parse(open(path, 'rb').read())
            print(path, hdr['class'], len(classes), 'classes')
        except Exception as e:  # noqa: BLE001
            print(path, 'ERROR', e)
