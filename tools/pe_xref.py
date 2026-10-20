"""Find a string in a PE and every instruction that references its VA; disassemble around each."""
import sys, pefile, capstone
path, needle = sys.argv[1], sys.argv[2].encode()
before = int(sys.argv[3], 0) if len(sys.argv) > 3 else 0x60
after = int(sys.argv[4], 0) if len(sys.argv) > 4 else 0x60
pe = pefile.PE(path); base = pe.OPTIONAL_HEADER.ImageBase
data = pe.get_memory_mapped_image()
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
pos = data.find(needle)
while pos >= 0:
    va = base + pos
    print(f"string @ {va:#x}")
    ref = data.find(va.to_bytes(4, 'little'))
    while ref >= 0:
        print(f"  ref @ {base+ref:#x}")
        start = ref - before
        for ins in md.disasm(data[start:ref + after], base + start):
            print(f"    {ins.address:#x}: {ins.mnemonic} {ins.op_str}")
        ref = data.find(va.to_bytes(4, 'little'), ref + 1)
    pos = data.find(needle, pos + 1)
