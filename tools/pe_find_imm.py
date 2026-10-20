"""Disassemble .text of a PE and print context around instructions using an immediate."""
import sys, pefile, capstone
path, imm = sys.argv[1], int(sys.argv[2], 0)
ctx = int(sys.argv[3]) if len(sys.argv) > 3 else 25
pe = pefile.PE(path); base = pe.OPTIONAL_HEADER.ImageBase
text = next(s for s in pe.sections if s.Name.startswith(b'.text'))
code = text.get_data(); start = base + text.VirtualAddress
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
insns = list(md.disasm(code, start))
for i, ins in enumerate(insns):
    if f"{imm:#x}" in ins.op_str:
        print(f"--- hit @ {ins.address:#x}")
        for j in insns[max(0, i - ctx):i + 8]:
            print(f"  {j.address:#x}: {j.mnemonic} {j.op_str}")
