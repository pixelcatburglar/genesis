"""Determine whether blue.dll needs blue_patcher: look for any signature-verification machinery."""
import sys, struct, pefile

path = sys.argv[1]
data = open(path, 'rb').read()
pe = pefile.PE(path)
print("size", len(data), "timestamp 0x%08x" % pe.FILE_HEADER.TimeDateStamp)
n = 0
for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', []):
    names = [(i.name or b'#%d' % i.ordinal).decode() for i in d.imports]
    n += len(names)
    print(d.dll.decode(), len(names), [x for x in names if 'Crypt' in x or 'Reg' in x][:20])
print("total imports", n)
print("delay imports:", hasattr(pe, 'DIRECTORY_ENTRY_DELAY_IMPORT'))
for needle in [b'Crypt', b'cryptoPack', b'Placebo', b'CryptoAPI', b'advapi', b'ADVAPI', struct.pack('<I', 0x80090006)]:
    print(repr(needle), data.count(needle))
